//! Real CLI subprocesses, private TLS cluster and actual Docker. Explicit opt-in; missing tools fail.
use anyhow::{Context, Result, ensure};
use gridstator::{client::Client, protocol, state};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Stdio, time::Duration};
use tempfile::TempDir;
use tokio::process::{Child, Command};

struct Cluster {
    root: TempDir,
    controller: Child,
    worker: Child,
    client: Client,
}
fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .current_dir(root.join("project"))
        .kill_on_drop(true)
        .env("GRIDSTATOR_CONFIG", root.join("client.json"))
        .env("GRIDSTATOR_CONTROLLER_DATA", root.join("controller"))
        .env("GRIDSTATOR_AGENT_DATA", root.join("agent"))
        .env_remove("GRIDSTATOR_URL")
        .env_remove("GRIDSTATOR_TOKEN")
        .env_remove("GRIDSTATOR_CA_FILE");
    command
}
async fn cli(root: &Path, args: &[&str], expected: i32) -> Result<String> {
    let output =
        tokio::time::timeout(Duration::from_secs(180), command(root).args(args).output()).await??;
    ensure!(
        output.status.code() == Some(expected),
        "CLI {args:?} exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}
async fn json_cli(root: &Path, args: &[&str]) -> Result<Value> {
    Ok(serde_json::from_str(&cli(root, args, 0).await?)?)
}
fn daemon(root: &Path, role: &str) -> Result<Child> {
    let log = fs::File::create(root.join(format!("{role}.log")))?;
    Ok(command(root)
        .args([role, "start"])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?)
}
impl Cluster {
    async fn boot() -> Result<Self> {
        let tool = Command::new("docker")
            .args(["info"])
            .output()
            .await
            .context("Docker must be installed for this proof")?;
        ensure!(
            tool.status.success(),
            "Docker daemon must be ready for this proof"
        );
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("project"))?;
        fs::write(
            root.path().join("project/source.txt"),
            "exact-uncommitted-native\n",
        )?;
        fs::write(
            root.path().join("project/.env"),
            "NEVER_UPLOAD_THIS=secret\n",
        )?;
        cli(
            root.path(),
            &["controller", "init", "--host", "127.0.0.1", "--port", "0"],
            0,
        )
        .await?;
        let controller = daemon(root.path(), "controller")?;
        let runtime = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(runtime) = state::read_json(&root.path().join("controller/runtime.json"))
                {
                    break runtime;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?;
        let config_path = root.path().join("client.json");
        let mut config = state::read_json(&config_path)?;
        config["url"] = json!(format!(
            "https://127.0.0.1:{}",
            runtime["port"].as_u64().context("bound port")?
        ));
        state::write_json(&config_path, &config)?;
        let client = Client::new(serde_json::from_value(config.clone())?)?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client.get("/v1/health").await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?;
        let token = json_cli(root.path(), &["token", "create", "--join", "--json"]).await?;
        cli(
            root.path(),
            &[
                "agent",
                "join",
                config["url"].as_str().unwrap(),
                "--token",
                token["token"].as_str().context("join token")?,
                "--fingerprint",
                config["caFingerprint"].as_str().unwrap(),
                "--name",
                "isolated-native-e2e",
            ],
            0,
        )
        .await?;
        let worker = daemon(root.path(), "agent")?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let nodes = client.get("/v1/nodes").await?;
                if nodes["nodes"]
                    .as_array()
                    .is_some_and(|nodes| nodes.iter().any(|node| node["status"] == "online"))
                {
                    break Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await??;
        Ok(Self {
            root,
            controller,
            worker,
            client,
        })
    }
    async fn terminal(&self, id: &str) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let job = self.client.get(&format!("/v1/jobs/{id}")).await?;
                if protocol::terminal(job["state"].as_str().unwrap_or("")) {
                    break Ok(job);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }
    async fn stop(mut self) -> Result<()> {
        self.worker.kill().await?;
        self.controller.kill().await?;
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires a ready Docker daemon; run cargo test --test cluster_e2e -- --ignored --nocapture"]
async fn actual_cli_snapshot_shards_exit_codes_artifacts_and_cancel() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let root = cluster.root.path();
    let output = cli(
        root,
        &[
            "run",
            "--image",
            "alpine:3.21",
            "--cpus",
            "1",
            "--memory",
            "64m",
            "--",
            "sh",
            "-c",
            "cat source.txt; test ! -e .env",
        ],
        0,
    )
    .await?;
    ensure!(
        output.contains("exact-uncommitted-native"),
        "snapshot bytes missing from logs"
    );
    cli(
        root,
        &[
            "run",
            "--image",
            "alpine:3.21",
            "--cpus",
            "1",
            "--memory",
            "64m",
            "--",
            "sh",
            "-c",
            "exit 7",
        ],
        7,
    )
    .await?;
    let output = cli(
        root,
        &[
            "run",
            "--image",
            "alpine:3.21",
            "--cpus",
            "1",
            "--memory",
            "64m",
            "--shards",
            "2",
            "--",
            "sh",
            "-c",
            "printf 'shard=%s/%s\\n' \"$GRIDSTATOR_SHARD_INDEX\" \"$GRIDSTATOR_SHARD_TOTAL\"",
        ],
        0,
    )
    .await?;
    ensure!(
        output.contains("shard=1/2") && output.contains("shard=2/2"),
        "shard environments/logs: {output}"
    );
    let job = json_cli(
        root,
        &[
            "run",
            "--image",
            "alpine:3.21",
            "--cpus",
            "1",
            "--memory",
            "64m",
            "--artifact",
            "result.txt",
            "--json",
            "--",
            "sh",
            "-c",
            "printf artifact-bytes > result.txt",
        ],
    )
    .await?;
    let id = job["id"].as_str().context("job id")?;
    let job = cluster.terminal(id).await?;
    ensure!(job["state"] == "succeeded", "artifact job failed: {job}");
    let artifacts = cluster
        .client
        .get(&format!("/v1/jobs/{id}/artifacts"))
        .await?;
    ensure!(
        artifacts["artifacts"]
            .as_array()
            .is_some_and(|list| !list.is_empty()),
        "artifact metadata missing"
    );
    let artifact = &artifacts["artifacts"][0];
    let target = root.join("downloaded.tar");
    cli(
        root,
        &[
            "artifacts",
            id,
            "--download",
            artifact["name"].as_str().context("artifact name")?,
            "--out",
            target.to_str().unwrap(),
        ],
        0,
    )
    .await?;
    let mut archive = tar::Archive::new(fs::File::open(&target)?);
    let mut matched = false;
    let mut names = vec![];
    for entry in archive.entries()? {
        let mut entry = entry?;
        names.push(entry.path()?.into_owned());
        if entry.path()?.ends_with("result.txt") {
            let mut bytes = String::new();
            use std::io::Read;
            entry.read_to_string(&mut bytes)?;
            ensure!(bytes == "artifact-bytes", "downloaded artifact changed");
            matched = true;
        }
    }
    ensure!(
        matched,
        "requested artifact absent from downloaded tar: {artifact}; members: {names:?}"
    );
    let job = json_cli(
        root,
        &[
            "run",
            "--image",
            "alpine:3.21",
            "--cpus",
            "1",
            "--memory",
            "64m",
            "--json",
            "--",
            "sleep",
            "120",
        ],
    )
    .await?;
    let id = job["id"].as_str().context("cancel job id")?;
    cli(root, &["cancel", id], 0).await?;
    ensure!(
        cluster.terminal(id).await?["state"] == "cancelled",
        "cancelled command did not finish"
    );
    // A helper request bound to another UI cluster must fail before any work is uploaded.
    let request = json!({"version":1,"operation":"capture","root":root.join("project"),"expectedClusterId":"another-cluster"});
    let mut bridge = command(root)
        .args(["bridge"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    use tokio::io::AsyncWriteExt;
    bridge
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request)?)
        .await?;
    let output = bridge.wait_with_output().await?;
    ensure!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("controller changed"),
        "bridge cluster guard failed"
    );
    cluster.stop().await
}

#[tokio::test]
#[ignore = "requires Docker/buildx/act/git; run cargo test --test cluster_e2e -- --ignored --nocapture"]
async fn actual_cli_build_and_actions_preserve_snapshot_outputs_and_reports() -> Result<()> {
    for (tool, args) in [
        ("docker", vec!["buildx", "version"]),
        ("act", vec!["--version"]),
        ("git", vec!["--version"]),
    ] {
        ensure!(
            Command::new(tool)
                .args(args)
                .output()
                .await?
                .status
                .success(),
            "required {tool} unavailable"
        );
    }
    let cluster = Cluster::boot().await?;
    let root = cluster.root.path();
    let project = root.join("project");
    fs::write(
        project.join("Dockerfile"),
        "FROM scratch\nCOPY source.txt /source.txt\n",
    )?;
    let build = json_cli(
        root,
        &[
            "build", "--output", "tar", "--cpus", "1", "--memory", "128m", "--json",
        ],
    )
    .await?;
    let id = build["id"].as_str().context("build job id")?;
    let finished = cluster.terminal(id).await?;
    ensure!(
        finished["state"] == "succeeded",
        "native build failed: {finished}"
    );
    ensure!(
        finished["outputs"]["image"]["output"] == "tar",
        "image output missing: {finished}"
    );
    ensure!(
        finished["artifacts"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["name"] == "image.tar")),
        "build archive absent"
    );
    fs::create_dir_all(project.join(".github/workflows"))?;
    fs::write(
        project.join(".github/workflows/native.yml"),
        "name: Native E2E\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - name: Exact snapshot\n        run: cat source.txt\n      - name: Test report\n        run: |\n          mkdir -p reports\n          printf '<testsuite tests=\"1\"><testcase name=\"native\" time=\"0.001\"/></testsuite>' > reports/junit.xml\n      - uses: actions/upload-artifact@v4\n        with: {name: test-report, path: reports/}\n",
    )?;
    let actions = json_cli(
        root,
        &[
            "actions",
            "run",
            "native.yml",
            "--platform",
            "ubuntu-latest=node:24-bookworm-slim",
            "--cpus",
            "1",
            "--memory",
            "512m",
            "--json",
        ],
    )
    .await?;
    let id = actions["id"].as_str().context("Actions job id")?;
    let finished = cluster.terminal(id).await?;
    ensure!(
        finished["state"] == "succeeded",
        "native Actions failed: {finished}"
    );
    ensure!(
        finished["outputs"]["steps"]
            .as_array()
            .is_some_and(|steps| steps
                .iter()
                .any(|step| step["name"] == "Exact snapshot" && step["status"] == "success")),
        "Actions step outputs absent"
    );
    let report = json_cli(root, &["job", id, "--json"]).await?;
    ensure!(
        report["job"]["id"] == id && report["events"].is_array(),
        "CLI job JSON contract: {report}"
    );
    let logs = cli(root, &["logs", id], 0).await?;
    ensure!(
        logs.contains("exact-uncommitted-native"),
        "Actions did not see exact snapshot: {logs}"
    );
    cluster.stop().await
}
