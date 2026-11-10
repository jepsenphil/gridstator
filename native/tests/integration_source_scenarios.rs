//! Exact integration source scenarios using an immutable CLI, private TLS state and two real
//! Docker workers. No existing services/clusters or resources without matching ownership are used.
use anyhow::{Context, Result, bail, ensure};
use gridstator::{protocol, state};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::{Child, Command};

const IMAGE: &str = "alpine:3.20";
const WAIT: Duration = Duration::from_secs(60);
struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}
fn redacted(text: &str) -> String {
    static TOKENS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    TOKENS
        .get_or_init(|| regex::Regex::new(r"gs[acj]_[A-Za-z0-9_-]+").unwrap())
        .replace_all(text, "<redacted-token>")
        .into_owned()
}
fn safe_args(args: &[&str]) -> Vec<String> {
    let mut secret = false;
    args.iter()
        .map(|arg| {
            let value = if secret {
                "<redacted-token>".into()
            } else {
                (*arg).to_owned()
            };
            secret = *arg == "--token";
            value
        })
        .collect()
}
async fn docker(args: &[&str]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(180),
        Command::new("docker")
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}
fn git(directory: &Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()?;
    ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(!kind.is_symlink(), "example fixture contains a link");
        let output = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &output)?;
        } else {
            fs::copy(entry.path(), output)?;
        }
    }
    Ok(())
}
struct Cluster {
    root: PathBuf,
    binary: PathBuf,
    roles: BTreeMap<String, Child>,
    node_ids: Vec<String>,
    keep: bool,
}
impl Drop for Cluster {
    fn drop(&mut self) {
        for child in self.roles.values_mut() {
            let _ = child.start_kill();
        }
        // Failure cleanup is restricted to this fixture's registered node identities.
        for id in &self.node_ids {
            for (kind, list) in [
                ("container", vec!["ps", "-aq"]),
                ("volume", vec!["volume", "ls", "-q"]),
            ] {
                let output = std::process::Command::new("docker")
                    .args(list)
                    .args([
                        "--filter",
                        "label=io.gridstator.managed=true",
                        "--filter",
                        &format!("label=io.gridstator.node={id}"),
                    ])
                    .output();
                if let Ok(output) = output {
                    for resource in String::from_utf8_lossy(&output.stdout).split_whitespace() {
                        let mut command = std::process::Command::new("docker");
                        if kind == "container" {
                            command.args(["rm", "-f", resource]);
                        } else {
                            command.args(["volume", "rm", resource]);
                        }
                        let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
                    }
                }
            }
        }
    }
}
impl Cluster {
    fn command(&self, args: &[&str], cwd: &Path) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .current_dir(cwd)
            .kill_on_drop(true)
            .env("GRIDSTATOR_CONFIG", self.root.join("client.json"))
            .env("GRIDSTATOR_CONTROLLER_DATA", self.root.join("controller"))
            .env("GRIDSTATOR_AGENT_DATA", self.root.join("unused-agent"))
            .env("GRIDSTATOR_CACHE", self.root.join("cache"))
            .env("NO_COLOR", "1")
            .env_remove("GRIDSTATOR_URL")
            .env_remove("GRIDSTATOR_TOKEN")
            .env_remove("GRIDSTATOR_CA_FILE")
            .env_remove("GRIDSTATOR_INSTALL_ROOT")
            .env_remove("GRIDSTATOR_INSTALL_VERSION")
            .env_remove("GRIDSTATOR_INSTALL_TARGET")
            .env_remove("GRIDSTATOR_SUPERVISED")
            .env_remove("GRIDSTATOR_BUILD_ID")
            .env_remove("GRIDSTATOR_DOCKER");
        command
    }
    async fn output(&self, args: &[&str], cwd: &Path, extra: &[(&str, &str)]) -> Result<Output> {
        let output = tokio::time::timeout(
            Duration::from_secs(300),
            self.command(args, cwd).envs(extra.iter().copied()).output(),
        )
        .await
        .with_context(|| format!("CLI {:?} timed out", safe_args(args)))??;
        Ok(Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8(output.stderr)?,
        })
    }
    async fn cli_at(&self, args: &[&str], cwd: &Path, expected: i32) -> Result<Output> {
        let output = self.output(args, cwd, &[]).await?;
        ensure!(
            output.code == expected,
            "CLI {:?} exit {}; stdout {}; stderr {}",
            safe_args(args),
            output.code,
            redacted(&output.stdout),
            redacted(&output.stderr)
        );
        Ok(output)
    }
    async fn cli(&self, args: &[&str], expected: i32) -> Result<Output> {
        self.cli_at(args, &self.root, expected).await
    }
    async fn json(&self, args: &[&str]) -> Result<Value> {
        let mut args = args.to_vec();
        args.push("--json");
        Ok(serde_json::from_str(&self.cli(&args, 0).await?.stdout)?)
    }
    async fn job(&self, id: &str) -> Result<Value> {
        Ok(self.json(&["job", id]).await?["job"].clone())
    }
    async fn state(&self, id: &str, wanted: &str) -> Result<Value> {
        tokio::time::timeout(WAIT, async {
            loop {
                let job = self.job(id).await?;
                if job["state"] == wanted {
                    return Ok(job);
                }
                ensure!(
                    !protocol::terminal(job["state"].as_str().unwrap_or("")),
                    "job {id} ended {} instead of {wanted}: {}",
                    job["state"],
                    job["error"]
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .with_context(|| format!("waiting for {id} state {wanted}"))?
    }
    async fn detach(&self, args: &[&str]) -> Result<String> {
        let mut all = vec!["run", "--detach"];
        all.extend_from_slice(args);
        Ok(self.cli(&all, 0).await?.stdout.trim().into())
    }
    fn start(&mut self, name: &str, args: &[&str]) -> Result<()> {
        ensure!(
            !self.roles.contains_key(name),
            "role {name} already running"
        );
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(format!("{name}.log")))?;
        let child = self
            .command(args, &self.root)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        self.roles.insert(name.into(), child);
        Ok(())
    }
    async fn stop(&mut self, name: &str, signal: i32) -> Result<()> {
        if let Some(mut child) = self.roles.remove(name)
            && child.try_wait()?.is_none()
        {
            if let Some(pid) = child.id() {
                // Signal only this fixture's recorded child, never a process-name match.
                ensure!(
                    unsafe { libc::kill(pid as i32, signal) } == 0,
                    "could not stop owned role {name}"
                );
            }
            match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
                Ok(status) => {
                    status?;
                }
                Err(_) => {
                    child.kill().await?;
                    bail!("owned role {name} did not stop gracefully");
                }
            }
        }
        Ok(())
    }
    async fn ready(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if self
                    .output(&["status", "--json"], &self.root, &[])
                    .await
                    .is_ok_and(|output| output.code == 0)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?;
        Ok(())
    }
    async fn both_online(&self) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let nodes = self.json(&["nodes"]).await?;
                if nodes.as_array().is_some_and(|nodes| {
                    nodes.len() == 2 && nodes.iter().all(|node| node["status"] == "online")
                }) {
                    return Ok(nodes);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }
    async fn boot() -> Result<Self> {
        for (program, args) in [
            ("docker", vec!["info"]),
            ("docker", vec!["buildx", "version"]),
            ("git", vec!["--version"]),
            ("act", vec!["--version"]),
        ] {
            let output = Command::new(program)
                .args(&args)
                .output()
                .await
                .with_context(|| format!("{program} is required"))?;
            ensure!(output.status.success(), "{program} {args:?} is required");
        }
        docker(&["pull", "-q", IMAGE]).await?;
        let root = tempfile::Builder::new()
            .prefix("gridstator-integration-source-")
            .tempdir()?
            .keep();
        state::private_dir(&root)?;
        let binary = root.join("gridstator");
        fs::copy(env!("CARGO_BIN_EXE_gridstator"), &binary)?;
        let mut cluster = Self {
            root,
            binary,
            roles: BTreeMap::new(),
            node_ids: Vec::new(),
            keep: std::env::var_os("GRIDSTATOR_KEEP_INTEGRATION_SOURCE_EVIDENCE").is_some(),
        };
        cluster
            .cli(
                &["controller", "init", "--host", "127.0.0.1", "--port", "0"],
                0,
            )
            .await?;
        cluster.start("controller", &["controller", "start"])?;
        let runtime = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(runtime) = state::read_json(&cluster.root.join("controller/runtime.json"))
                    && runtime["pid"].as_u64() == cluster.roles["controller"].id().map(u64::from)
                {
                    break runtime;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?;
        let port = runtime["port"].as_u64().context("bound port")?;
        let config_path = cluster.root.join("client.json");
        let mut config = state::read_json(&config_path)?;
        config["url"] = json!(format!("https://127.0.0.1:{port}"));
        state::write_json(&config_path, &config)?;
        // Preserve the actual bound endpoint for the restart scenario and both workers.
        let controller_config = cluster.root.join("controller/controller.json");
        state::update_json(&controller_config, |config| {
            config["port"] = json!(port);
            Ok(())
        })?;
        cluster.ready().await?;
        cluster
            .cli(
                &["agents", "set", "--auto-update", "off", "--act", "off"],
                0,
            )
            .await?;
        let join = cluster
            .json(&["token", "create", "--join", "--uses", "2"])
            .await?;
        for (name, labels) in [("alpha", "fast"), ("beta", "slow")] {
            let data = cluster.root.join(name);
            cluster
                .cli(
                    &[
                        "agent",
                        "join",
                        config["url"].as_str().unwrap(),
                        "--token",
                        join["token"].as_str().unwrap(),
                        "--fingerprint",
                        config["caFingerprint"].as_str().unwrap(),
                        "--name",
                        name,
                        "--data-dir",
                        data.to_str().unwrap(),
                        "--labels",
                        labels,
                    ],
                    0,
                )
                .await?;
            let saved = state::read_json(&data.join("agent.json"))?;
            cluster
                .node_ids
                .push(saved["nodeId"].as_str().context("node id")?.into());
            cluster.start(
                name,
                &["agent", "start", "--data-dir", data.to_str().unwrap()],
            )?;
        }
        let nodes = cluster.both_online().await?;
        state::write_json(&cluster.root.join("nodes-before.json"), &nodes)?;
        let build = cluster.json(&["build-info"]).await?;
        state::write_json(&cluster.root.join("cli-build-info.json"), &build)?;
        eprintln!("Integration source evidence: {}", cluster.root.display());
        Ok(cluster)
    }
    async fn cleanup(&mut self) -> Result<()> {
        for name in ["alpha", "beta", "controller"] {
            self.stop(name, libc::SIGTERM).await?;
        }
        for id in &self.node_ids {
            ensure!(
                docker(&[
                    "ps",
                    "-aq",
                    "--filter",
                    &format!("label=io.gridstator.node={id}")
                ])
                .await?
                .trim()
                .is_empty(),
                "owned node {id} left containers"
            );
            ensure!(
                docker(&[
                    "volume",
                    "ls",
                    "-q",
                    "--filter",
                    &format!("label=io.gridstator.node={id}")
                ])
                .await?
                .trim()
                .is_empty(),
                "owned node {id} left workspace volumes"
            );
        }
        Ok(())
    }
}

async fn source_001(cluster: &mut Cluster) -> Result<Value> {
    let output = cluster
        .cli(
            &[
                "run",
                "--no-snapshot",
                "--image",
                IMAGE,
                "--node",
                "beta",
                "--",
                "sh",
                "-c",
                r#"echo "hi from $GRIDSTATOR_NODE_NAME"; echo oops >&2; exit 7"#,
            ],
            7,
        )
        .await?;
    ensure!(
        output.stdout.contains("hi from beta"),
        "chosen-node stdout: {}",
        output.stdout
    );
    ensure!(
        output.stderr.contains("oops") && output.stderr.contains("failed on beta"),
        "chosen-node stderr: {}",
        output.stderr
    );
    Ok(json!({"exitCode":output.code,"stdout":output.stdout,"stderr":redacted(&output.stderr)}))
}
async fn source_002(cluster: &mut Cluster) -> Result<Value> {
    let project = cluster.root.join("project");
    fs::create_dir_all(project.join("src"))?;
    fs::write(project.join("src/value.txt"), "committed\n")?;
    fs::write(project.join(".gitignore"), "build/\n")?;
    git(&project, &["init", "-q"])?;
    git(
        &project,
        &["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"],
    )?;
    git(
        &project,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ],
    )?;
    fs::write(project.join("src/value.txt"), "uncommitted edit\n")?;
    fs::write(project.join("src/new.txt"), "untracked file\n")?;
    fs::write(project.join(".env"), "TOKEN=supersecret\n")?;
    fs::create_dir(project.join("build"))?;
    fs::write(project.join("build/out.txt"), "ignored\n")?;
    let output = cluster
        .cli_at(
            &[
                "run",
                "--image",
                IMAGE,
                "--",
                "sh",
                "-c",
                r#"cat value.txt new.txt; test ! -e ../.env && echo "no .env"; test ! -e ../build && echo "no build"; pwd"#,
            ],
            &project.join("src"),
            0,
        )
        .await?;
    ensure!(
        output.stdout == "uncommitted edit\nuntracked file\nno .env\nno build\n/workspace/src\n",
        "snapshot stdout: {:?}",
        output.stdout
    );
    ensure!(
        output.stderr.contains(".env: excluded"),
        "missing secret exclusion diagnostic: {}",
        output.stderr
    );
    let snapshot: Value = serde_json::from_str(
        &cluster
            .cli_at(&["snapshot", "--dry-run", "--json"], &project, 0)
            .await?
            .stdout,
    )?;
    ensure!(
        snapshot["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>()
            == [".gitignore", "src/new.txt", "src/value.txt"],
        "snapshot entries: {}",
        snapshot["entries"]
    );
    let again = cluster.cli_at(&["snapshot"], &project, 0).await?;
    ensure!(
        again.stdout.trim() == snapshot["id"].as_str().unwrap(),
        "snapshot id changed"
    );
    ensure!(
        again.stderr.contains("nothing to upload"),
        "duplicate snapshot diagnostic: {}",
        again.stderr
    );
    Ok(
        json!({"snapshot":snapshot["id"],"entries":snapshot["entries"],"stdout":output.stdout,"secondUpload":again.stderr}),
    )
}
async fn source_003(cluster: &mut Cluster) -> Result<Value> {
    let output = cluster
        .output(
            &[
                "run",
                "--no-snapshot",
                "--image",
                IMAGE,
                "--secret-env",
                "MY_SECRET",
                "--",
                "sh",
                "-c",
                r#"echo "value=$MY_SECRET""#,
            ],
            &cluster.root,
            &[("MY_SECRET", "p4ssw0rd-123")],
        )
        .await?;
    ensure!(
        output.code == 0 && output.stdout == "value=***\n",
        "secret masking did not preserve source contract"
    );
    let jobs = cluster.json(&["jobs", "--limit", "1"]).await?;
    let detail = cluster
        .cli(&["job", jobs[0]["id"].as_str().unwrap(), "--json"], 0)
        .await?;
    ensure!(
        !detail.stdout.contains("p4ssw0rd-123"),
        "secret leaked in job details"
    );
    Ok(json!({"jobId":jobs[0]["id"],"stdout":output.stdout,"publicDetailRedacted":true}))
}
async fn source_004(cluster: &mut Cluster) -> Result<Value> {
    let a = cluster
        .detach(&["--no-snapshot", "--image", IMAGE, "--", "sleep", "4"])
        .await?;
    let b = cluster
        .detach(&["--no-snapshot", "--image", IMAGE, "--", "sleep", "4"])
        .await?;
    let (a, b) = tokio::try_join!(
        cluster.state(&a, "succeeded"),
        cluster.state(&b, "succeeded")
    )?;
    ensure!(
        HashSet::from([
            a["nodeName"].as_str().unwrap(),
            b["nodeName"].as_str().unwrap()
        ]) == HashSet::from(["alpha", "beta"]),
        "jobs did not spread: {a} {b}"
    );
    ensure!(
        a["startedAt"]
            .as_i64()
            .unwrap()
            .max(b["startedAt"].as_i64().unwrap())
            < a["finishedAt"]
                .as_i64()
                .unwrap()
                .min(b["finishedAt"].as_i64().unwrap()),
        "executions did not overlap"
    );
    Ok(json!({"jobs":[a,b],"overlapped":true}))
}
async fn source_005(cluster: &mut Cluster) -> Result<Value> {
    let fast = cluster
        .detach(&[
            "--no-snapshot",
            "--image",
            IMAGE,
            "-l",
            "fast",
            "--",
            "true",
        ])
        .await?;
    ensure!(
        cluster.state(&fast, "succeeded").await?["nodeName"] == "alpha",
        "fast label selected wrong worker"
    );
    let none = cluster
        .detach(&["--no-snapshot", "--image", IMAGE, "-l", "gpu", "--", "true"])
        .await?;
    let job = cluster.job(&none).await?;
    ensure!(
        job["stateReason"]
            .as_str()
            .is_some_and(|reason| reason.contains("label(s) gpu")),
        "pending reason: {}",
        job["stateReason"]
    );
    cluster.cli(&["cancel", &none], 0).await?;
    cluster.state(&none, "cancelled").await?;
    Ok(json!({"fastJob":fast,"queuedJob":none,"pendingReason":job["stateReason"]}))
}
async fn source_006(cluster: &mut Cluster) -> Result<Value> {
    let id = cluster
        .detach(&[
            "--no-snapshot",
            "--image",
            IMAGE,
            "--",
            "sh",
            "-c",
            "echo started; sleep 300",
        ])
        .await?;
    tokio::time::timeout(WAIT, async {
        loop {
            if cluster
                .cli(&["logs", &id], 0)
                .await?
                .stdout
                .contains("started")
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let began = Instant::now();
    cluster.cli(&["cancel", &id], 0).await?;
    cluster.state(&id, "cancelled").await?;
    let cancel_elapsed = began.elapsed();
    ensure!(
        cancel_elapsed < Duration::from_secs(15),
        "cancellation exceeded source deadline"
    );
    ensure!(
        docker(&[
            "ps",
            "-aq",
            "--filter",
            &format!("label=io.gridstator.job={id}")
        ])
        .await?
        .trim()
        .is_empty(),
        "cancelled container remained"
    );
    let retry = cluster
        .cli(&["retry", &id, "--detach"], 0)
        .await?
        .stdout
        .trim()
        .to_owned();
    let running = cluster.state(&retry, "running").await?;
    ensure!(
        running["attempt"] == 2 && running["retryOf"] == id,
        "retry lineage changed: {running}"
    );
    cluster.cli(&["cancel", &retry], 0).await?;
    cluster.state(&retry, "cancelled").await?;
    Ok(
        json!({"jobId":id,"retryId":retry,"attempt":2,"cancelMilliseconds":cancel_elapsed.as_millis()}),
    )
}
async fn source_007(cluster: &mut Cluster) -> Result<Value> {
    let id = cluster
        .detach(&[
            "--no-snapshot",
            "--image",
            IMAGE,
            "--node",
            "alpha",
            "--",
            "sh",
            "-c",
            r#"for i in $(seq 1 12); do echo "line $i"; sleep 0.5; done"#,
        ])
        .await?;
    tokio::time::timeout(WAIT, async {
        loop {
            if cluster
                .cli(&["logs", &id], 0)
                .await?
                .stdout
                .contains("line 2")
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    cluster.stop("controller", libc::SIGTERM).await?;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    cluster.start("controller", &["controller", "start"])?;
    cluster.ready().await?;
    let done = cluster.state(&id, "succeeded").await?;
    ensure!(
        done["nodeName"] == "alpha",
        "job changed worker across restart"
    );
    let output = cluster.cli(&["logs", &id], 0).await?.stdout;
    ensure!(
        output == (1..=12).map(|n| format!("line {n}\n")).collect::<String>(),
        "restart logs changed: {output:?}"
    );
    cluster.both_online().await?;
    Ok(json!({"jobId":id,"node":"alpha","stdout":output,"bothWorkersReconnected":true}))
}
struct Foreign {
    container: String,
    volume: String,
}
struct ImageTag(String);
impl Drop for ImageTag {
    fn drop(&mut self) {
        // The tag is unique to this fixture. Keep shared build layers and unrelated tags.
        let _ = std::process::Command::new("docker")
            .args(["image", "rm", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
impl Drop for Foreign {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::process::Command::new("docker")
            .args(["volume", "rm", &self.volume])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
async fn source_008(cluster: &mut Cluster) -> Result<Value> {
    let unique = state::id().to_lowercase();
    let foreign = Foreign {
        container: format!("gridstator-source-foreign-{unique}"),
        volume: format!("gridstator-source-foreign-volume-{unique}"),
    };
    let other_node = format!("unrelated-node-{unique}");
    let other_job = format!("unrelated-job-{unique}");
    docker(&[
        "volume",
        "create",
        "--label",
        "io.gridstator.managed=true",
        "--label",
        &format!("io.gridstator.node={other_node}"),
        "--label",
        &format!("io.gridstator.job={other_job}"),
        &foreign.volume,
    ])
    .await?;
    docker(&[
        "run",
        "-d",
        "--name",
        &foreign.container,
        "--cpus",
        "0.1",
        "--memory",
        "16m",
        "--label",
        "io.gridstator.managed=true",
        "--label",
        &format!("io.gridstator.node={other_node}"),
        "--label",
        &format!("io.gridstator.job={other_job}"),
        "--mount",
        &format!("type=volume,src={},dst=/guard", foreign.volume),
        IMAGE,
        "sleep",
        "600",
    ])
    .await?;
    let id = cluster
        .detach(&[
            "--no-snapshot",
            "--image",
            IMAGE,
            "--node",
            "beta",
            "--retries",
            "1",
            "--",
            "sh",
            "-c",
            r#"echo "attempt $GRIDSTATOR_ATTEMPT"; sleep 3"#,
        ])
        .await?;
    cluster.state(&id, "running").await?;
    // Prove cleanup of resources that really existed before the crash, rather than passing
    // if the worker was killed between its running transition and Docker creation.
    let (owned_container, owned_volume) = tokio::time::timeout(WAIT, async {
        loop {
            let container = docker(&[
                "ps",
                "-q",
                "--filter",
                &format!("label=io.gridstator.job={id}"),
                "--filter",
                "status=running",
            ])
            .await?;
            let volume = docker(&[
                "volume",
                "ls",
                "-q",
                "--filter",
                &format!("label=io.gridstator.job={id}"),
            ])
            .await?;
            if !container.trim().is_empty() && !volume.trim().is_empty() {
                return Ok::<_, anyhow::Error>((
                    container.trim().to_owned(),
                    volume.trim().to_owned(),
                ));
            }
            ensure!(
                !protocol::terminal(cluster.job(&id).await?["state"].as_str().unwrap_or("")),
                "job ended before the crash proof observed its Docker resources"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    cluster.stop("beta", libc::SIGKILL).await?;
    let data = cluster.root.join("beta");
    cluster.start(
        "beta",
        &["agent", "start", "--data-dir", data.to_str().unwrap()],
    )?;
    let lost = cluster.state(&id, "lost").await?;
    ensure!(
        lost["error"]
            .as_str()
            .is_some_and(|error| error.contains("restarted or lost track")),
        "lost diagnostic: {}",
        lost["error"]
    );
    ensure!(
        docker(&[
            "ps",
            "-aq",
            "--filter",
            &format!("label=io.gridstator.job={id}")
        ])
        .await?
        .trim()
        .is_empty(),
        "dead worker's orphan container survived"
    );
    ensure!(
        docker(&[
            "volume",
            "ls",
            "-q",
            "--filter",
            &format!("label=io.gridstator.job={id}")
        ])
        .await?
        .trim()
        .is_empty(),
        "dead worker's workspace volume survived"
    );
    ensure!(
        docker(&[
            "inspect",
            "--format",
            "{{.State.Running}}",
            &foreign.container
        ])
        .await?
        .trim()
            == "true",
        "other node's unrelated container was removed"
    );
    ensure!(
        docker(&[
            "volume",
            "inspect",
            "--format",
            "{{.Name}}",
            &foreign.volume
        ])
        .await?
        .trim()
            == foreign.volume,
        "other node's unrelated volume was removed"
    );
    let retried = tokio::time::timeout(WAIT, async {
        loop {
            let jobs = cluster.json(&["jobs", "--limit", "5"]).await?;
            if let Some(job) = jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["retryOf"] == id)
            {
                return Ok::<_, anyhow::Error>(job.clone());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let final_job = cluster
        .state(retried["id"].as_str().unwrap(), "succeeded")
        .await?;
    ensure!(final_job["attempt"] == 2, "automatic retry attempt changed");
    let output = cluster
        .cli(&["logs", retried["id"].as_str().unwrap()], 0)
        .await?
        .stdout;
    ensure!(output == "attempt 2\n", "automatic retry logs: {output:?}");
    Ok(
        json!({"lost":lost,"retry":final_job,"stdout":output,"ownedContainerBeforeKill":owned_container,"ownedVolumeBeforeKill":owned_volume,"ownedContainerAndVolumeRemoved":true,"foreignNode":other_node,"foreignContainerSurvived":foreign.container,"foreignVolumeSurvived":foreign.volume}),
    )
}
async fn source_009(cluster: &mut Cluster) -> Result<Value> {
    let project = cluster.root.join("docker-build");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/docker-build"),
        &project,
    )?;
    let owned_tag = ImageTag(format!(
        "gridstator-native-source:{}",
        state::id().to_lowercase()
    ));
    let tag = &owned_tag.0;
    let output = cluster
        .cli_at(
            &[
                "build",
                "--node",
                "beta",
                "--target",
                "runner",
                "-t",
                tag,
                "--build-arg",
                "APP_VERSION=7.7.7",
            ],
            &project,
            0,
        )
        .await?;
    ensure!(
        output.stderr.contains("Image sha256:") && output.stderr.contains(&format!("({tag})")),
        "build presentation: {}",
        output.stderr
    );
    let jobs = cluster.json(&["jobs", "--limit", "1"]).await?;
    let built = &jobs[0];
    ensure!(built["nodeName"] == "beta", "build selected wrong worker");
    let label = docker(&[
        "image",
        "inspect",
        tag,
        "--format",
        r#"{{index .Config.Labels "io.gridstator.job"}}"#,
    ])
    .await?;
    ensure!(
        label.trim() == built["id"].as_str().unwrap(),
        "image lost job ownership"
    );
    ensure!(
        built["outputs"]["image"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("sha256:")),
        "image output missing"
    );
    docker(&["image", "rm", tag]).await?;
    Ok(
        json!({"jobId":built["id"],"node":"beta","image":built["outputs"]["image"],"ownershipLabel":label.trim(),"imageTagRemoved":tag,"stderr":redacted(&output.stderr)}),
    )
}
async fn source_010(cluster: &mut Cluster) -> Result<Value> {
    let project = cluster.root.join("sharded");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/sharded-tests"),
        &project,
    )?;
    let output = cluster
        .cli_at(&["run", "--task", "test"], &project, 0)
        .await?;
    ensure!(
        output.stdout.contains("[1/2]") && output.stdout.contains("[2/2]"),
        "shard prefixes missing: {}",
        output.stdout
    );
    let groups = cluster.json(&["groups", "--limit", "1"]).await?;
    let shown = cluster
        .json(&["group", "show", groups[0]["id"].as_str().unwrap()])
        .await?;
    let group = &shown["group"];
    ensure!(group["state"] == "succeeded", "group failed: {group}");
    ensure!(
        group["nodes"] == json!(["alpha", "beta"]),
        "shards did not span both workers: {}",
        group["nodes"]
    );
    let jobs = group["jobs"].as_array().unwrap();
    ensure!(
        jobs[0]["startedAt"]
            .as_i64()
            .unwrap()
            .max(jobs[1]["startedAt"].as_i64().unwrap())
            < jobs[0]["finishedAt"]
                .as_i64()
                .unwrap()
                .min(jobs[1]["finishedAt"].as_i64().unwrap()),
        "shards did not overlap"
    );
    ensure!(
        shown["tests"]["total"] == 8
            && shown["tests"]["passed"] == 8
            && shown["tests"]["failed"] == 0,
        "combined JUnit changed: {}",
        shown["tests"]
    );
    Ok(json!({"group":group,"tests":shown["tests"],"stdout":output.stdout,"overlapped":true}))
}
async fn source_011(cluster: &mut Cluster) -> Result<Value> {
    cluster.both_online().await?;
    let output = cluster.cli(&["doctor"], 0).await?;
    ensure!(
        output.stdout.contains("✓ controller") && output.stdout.contains("2/2 online"),
        "doctor is not healthy: {}",
        output.stdout
    );
    Ok(json!({"exitCode":output.code,"stdout":output.stdout,"stderr":redacted(&output.stderr)}))
}

async fn source_012(cluster: &mut Cluster) -> Result<Value> {
    let project = cluster.root.join("gha");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/github-actions"),
        &project,
    )?;
    git(&project, &["init", "-q", "-b", "main"])?;
    git(
        &project,
        &["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"],
    )?;
    git(
        &project,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ],
    )?;
    fs::write(
        project.join("src/greet.js"),
        "export function greet(name) {\n  return `Hi, ${name}!`;\n}\n",
    )?;
    fs::write(
        project.join("test/greet.test.js"),
        "import assert from 'node:assert/strict';\nimport { test } from 'node:test';\nimport { greet } from '../src/greet.js';\ntest('greet', () => assert.equal(greet('x'), 'Hi, x!'));\n",
    )?;
    let workflows: Value = serde_json::from_str(
        &cluster
            .cli_at(&["actions", "list", "--json"], &project, 0)
            .await?
            .stdout,
    )?;
    ensure!(
        workflows[0]["jobs"]
            .as_array()
            .context("discovered Actions jobs")?
            .iter()
            .map(|job| job["id"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            == ["test", "lint"],
        "Actions discovery changed: {workflows}"
    );
    let output = cluster
        .cli_at(
            &[
                "actions", "run", "ci.yml", "--job", "test", "--node", "alpha",
            ],
            &project,
            0,
        )
        .await?;
    ensure!(
        output.stdout.contains("[CI/test]   | ✔ greet"),
        "Actions did not run the dirty-tree greet test: {}",
        output.stdout
    );
    let jobs = cluster.json(&["jobs", "--limit", "1"]).await?;
    let ran = &jobs[0];
    ensure!(
        ran["outputs"]["steps"]
            .as_array()
            .context("Actions step outputs")?
            .iter()
            .any(|step| step["name"] == "Run tests" && step["status"] == "success"),
        "Run tests step did not succeed: {}",
        ran["outputs"]
    );
    ensure!(
        ran["artifacts"]
            .as_array()
            .context("Actions artifacts")?
            .iter()
            .map(|artifact| artifact["name"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            == ["test-report/test-report.zip"],
        "Actions artifact names changed: {}",
        ran["artifacts"]
    );
    let report = cluster.root.join("report.zip");
    cluster
        .cli(
            &[
                "artifacts",
                ran["id"].as_str().context("Actions job ID")?,
                "--download",
                "test-report/test-report.zip",
                "-o",
                report.to_str().context("report path")?,
            ],
            0,
        )
        .await?;
    let archive = fs::read(&report)?;
    ensure!(
        archive.starts_with(b"PK"),
        "download is not the expected ZIP"
    );
    Ok(json!({
        "job":ran,"workflows":workflows,"stdout":output.stdout,
        "stderr":redacted(&output.stderr),"artifactZipSignature":"PK",
        "artifactBytes":archive.len(),"dirtyTrackedFiles":["src/greet.js","test/greet.test.js"]
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real Docker/buildx/git/act; frozen conditional Actions source scenario"]
async fn conditional_actions_source_012_runs_dirty_tree_and_downloads_exact_artifact() -> Result<()>
{
    let mut cluster = Cluster::boot().await?;
    let started = Instant::now();
    let detail = source_012(&mut cluster)
        .await
        .with_context(|| format!("source012 private evidence {}", cluster.root.display()))?;
    state::write_json(
        &cluster.root.join("scenario-results.json"),
        &json!([{"id":"reference-integration-e2e-012","result":"passed","elapsedMs":started.elapsed().as_millis(),"observations":detail}]),
    )?;
    let nodes = cluster.both_online().await?;
    state::write_json(&cluster.root.join("nodes-after.json"), &nodes)?;
    cluster.cleanup().await?;
    state::write_json(
        &cluster.root.join("cleanup.json"),
        &json!({"ownedRolesStopped":true,"ownedContainersAndWorkspaceVolumesAbsent":true,"existingServicesTouched":false,"sharedCachesRetained":true}),
    )?;
    eprintln!(
        "Conditional Actions source012 passed; evidence {}",
        cluster.root.display()
    );
    if !cluster.keep {
        fs::remove_dir_all(&cluster.root)?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real Docker/buildx/git/act; runs the eleven frozen integration scenarios serially"]
async fn all_eleven_source_scenarios_with_two_real_native_workers() -> Result<()> {
    let mut cluster = Cluster::boot().await?;
    let selected = std::env::var("GRIDSTATOR_INTEGRATION_SOURCE_CASES")
        .ok()
        .map(|value| value.split(',').map(str::to_owned).collect::<HashSet<_>>());
    if let Some(selected) = &selected {
        ensure!(
            selected
                .iter()
                .all(|id| (1..=11).any(|n| *id == format!("{n:03}"))),
            "invalid selected source case"
        );
    }
    let mut results = Vec::new();
    macro_rules! scenario {
        ($id:literal, $function:ident) => {
            if selected
                .as_ref()
                .is_none_or(|selected| selected.contains($id))
            {
                eprintln!("Starting integration source {}", $id);
                let started = Instant::now();
                let detail = $function(&mut cluster).await.with_context(|| {
                    format!(
                        "integration source {} (private evidence {})",
                        $id,
                        cluster.root.display()
                    )
                })?;
                results.push(json!({
                    "id": concat!("reference-integration-e2e-", $id),
                    "result": "passed",
                    "elapsedMs": started.elapsed().as_millis(),
                    "observations": detail
                }));
                state::write_json(&cluster.root.join("scenario-results.json"), &json!(results))?;
                eprintln!(
                    "Passed integration source {} in {:.2}s",
                    $id,
                    started.elapsed().as_secs_f64()
                );
            }
        };
    }
    scenario!("001", source_001);
    scenario!("002", source_002);
    scenario!("003", source_003);
    scenario!("004", source_004);
    scenario!("005", source_005);
    scenario!("006", source_006);
    scenario!("007", source_007);
    scenario!("008", source_008);
    scenario!("009", source_009);
    scenario!("010", source_010);
    scenario!("011", source_011);
    let nodes = cluster.both_online().await?;
    state::write_json(&cluster.root.join("nodes-after.json"), &nodes)?;
    cluster.cleanup().await?;
    state::write_json(
        &cluster.root.join("cleanup.json"),
        &json!({"ownedRolesStopped":true,"ownedContainersAndWorkspaceVolumesAbsent":true,"existingServicesTouched":false,"sharedCachesRetained":true}),
    )?;
    eprintln!(
        "Integration source scenarios passed; evidence {}",
        cluster.root.display()
    );
    if !cluster.keep {
        fs::remove_dir_all(&cluster.root)?;
    }
    Ok(())
}
