//! Explicit root-only system-service fixture. Run the compiled ignored test on the authorized
//! disposable Linux host; all service names/state are private and supplied in a fixture document.
#![cfg(target_os = "linux")]
use anyhow::{Context, Result, ensure};
use gridstator::{
    client::{Client, Config},
    controller, db,
    install::InstallStore,
    release::{self, ArtifactMirror, ReleaseManifest},
    services::{
        ServiceAction, ServiceConfig, ServiceManager, ServicePlatform, ServiceRole, ServiceScope,
    },
    state,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inputs {
    root: PathBuf,
    unit: String,
    release_directory: PathBuf,
    bad_bundle: PathBuf,
    bad_build_id: String,
    #[serde(default)]
    controller_host: Option<String>,
    #[serde(default)]
    positive_release_directory: Option<PathBuf>,
    #[serde(default)]
    positive_bundle: Option<PathBuf>,
    #[serde(default)]
    external_worker_names: Vec<String>,
}
struct OwnedWorker(Child);
impl OwnedWorker {
    fn stop(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_none() {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            for _ in 0..400 {
                if self.0.try_wait()?.is_some() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            self.0.kill()?;
            self.0.wait()?;
            anyhow::bail!("owned standalone worker did not stop gracefully");
        }
        Ok(())
    }
}
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
struct OwnedService {
    manager: ServiceManager,
    config: ServiceConfig,
}
impl Drop for OwnedService {
    fn drop(&mut self) {
        let _ = self.manager.uninstall(&self.config);
        let _ = Command::new("systemctl")
            .args(["reset-failed", &self.config.unit_name()])
            .output();
    }
}
fn system(args: &[&str]) -> Result<String> {
    let output = Command::new("systemctl").args(args).output()?;
    ensure!(
        output.status.success(),
        "owned service command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}
fn existing_agent() -> Result<Value> {
    let state = system(&[
        "show",
        "gridstator-agent.service",
        "--property=ActiveState",
        "--property=SubState",
        "--property=UnitFileState",
        "--property=MainPID",
        "--property=InvocationID",
        "--property=FragmentPath",
        "--no-pager",
    ])?;
    let path = Path::new("/etc/systemd/system/gridstator-agent.service");
    Ok(json!({"state":state,"unitSha256":release::hash_file(path)?.1}))
}
fn facts(path: &Path) -> Result<Value> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let jobs: i64 = connection.query_row(
        "SELECT count(*) FROM records WHERE kind='jobs'",
        [],
        |row| row.get(0),
    )?;
    let candidate: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='candidate_schema'",
        [],
        |row| row.get(0),
    )?;
    let column = connection
        .prepare("PRAGMA table_info(records)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "candidate_only");
    Ok(
        json!({"userVersion":version,"jobs":jobs,"candidateTable":candidate>0,"candidateColumn":column}),
    )
}
async fn ready(
    data: &Path,
    original: &Config,
    host: &str,
    previous: Option<u64>,
) -> Result<(Client, u64)> {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if let Ok(runtime) = state::read_json(&data.join("runtime.json")) {
                let pid = runtime["pid"].as_u64().context("runtime PID")?;
                if Some(pid) != previous {
                    let mut config = original.clone();
                    config.url = format!(
                        "https://{host}:{}",
                        runtime["port"].as_u64().context("bound port")?
                    );
                    let client = Client::new(config)?;
                    if client.get("/v1/health").await.is_ok() {
                        return Ok::<_, anyhow::Error>((client, pid));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("owned controller did not become healthy")?
}
async fn publish_bundle(client: &Client, path: &Path) -> Result<Value> {
    let (_, digest) = release::hash_file(path)?;
    let response = client
        .http
        .post(format!(
            "{}/v1/controller/build?sha256={digest}",
            client.config.url
        ))
        .bearer_auth(&client.config.token)
        .header("content-type", "application/gzip")
        .body(fs::read(path)?)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    ensure!(status.is_success(), "controlled candidate rejected: {body}");
    Ok(serde_json::from_str(&body)?)
}
async fn named_workers(client: &Client, names: &[String], build: &str) -> Result<Vec<Value>> {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let nodes = client.get("/v1/nodes").await?;
            let all = nodes["nodes"].as_array().context("nodes view")?;
            let selected = names
                .iter()
                .filter_map(|name| {
                    all.iter()
                        .find(|node| {
                            node["name"] == name.as_str()
                                && node["status"] == "online"
                                && node["info"]["build"]["managed"] == true
                                && node["info"]["build"]["id"] == build
                        })
                        .cloned()
                })
                .collect::<Vec<_>>();
            if selected.len() == names.len() {
                return Ok::<_, anyhow::Error>(selected);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("private mixed-target workers did not join with expected managed identity")?
}
async fn start_lan_workers(
    input: &Inputs,
    client: &Client,
    baseline: &ReleaseManifest,
) -> Result<(OwnedWorker, Vec<Value>)> {
    ensure!(
        input.external_worker_names.len() == 2,
        "mixed proof requires Linux and Mac worker names"
    );
    let token = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"private-mixed-pairing","maxUses":2}),
        )
        .await?;
    let token = token["token"].as_str().context("private pairing token")?;
    let linux_data = input.root.join("linux-worker");
    gridstator::worker::join(
        &linux_data,
        &client.config.url,
        token,
        &client.config.ca_fingerprint,
        Some(input.external_worker_names[0].clone()),
        Some(json!({"autoUpdate":"off","maxConcurrency":1,"reserved":{"cpus":0,"memoryMb":0}})),
    )
    .await?;
    let mut pairing = client.config.clone();
    pairing.token = token.into();
    let mac=Client::new(pairing)?.post("/v1/agents/register",json!({"name":input.external_worker_names[1],"info":{"hostname":"private-mac-proof","platform":"darwin","arch":"arm64","osRelease":"fixture-initial-registration","cpus":8,"memoryTotalMb":8192,"agentVersion":baseline.version,"docker":{"available":false},"buildx":{"available":false},"act":{"available":false},"executors":[]},"settings":{"autoUpdate":"off","maxConcurrency":1,"reserved":{"cpus":0,"memoryMb":0}}})).await?;
    state::write_json(
        &input.root.join("mac-agent.json"),
        &json!({"version":2,"controllerUrl":client.config.url,"caPem":client.config.ca_pem,"caFingerprint":client.config.ca_fingerprint,"nodeId":mac["nodeId"],"name":mac["name"],"credential":mac["credential"],"settings":{"autoUpdate":"off","maxConcurrency":1,"reserved":{"cpus":0,"memoryMb":0}}}),
    )?;
    state::write_json(
        &input.root.join("lan-handoff.json"),
        &json!({"controllerUrl":client.config.url,"caFingerprint":client.config.ca_fingerprint,"baselineBuildId":baseline.build_id,"macNodeId":mac["nodeId"],"macName":mac["name"],"privateAgentConfig":input.root.join("mac-agent.json"),"target":"darwin/arm64"}),
    )?;
    let worker_store = InstallStore::new(input.root.join("linux-install"));
    let target = release::host_target()?;
    worker_store.install_verified(
        baseline,
        target,
        input
            .release_directory
            .join(&baseline.target(target)?.archive.file),
    )?;
    worker_store.activate(&baseline.build_id, None)?;
    let supervisor =
        worker_store.install_supervisor(worker_store.executable(&baseline.build_id)?, false)?;
    let log = fs::File::create(input.root.join("linux-worker.log"))?;
    let mut command = Command::new(supervisor);
    command
        .args(["supervise", "--install-root"])
        .arg(worker_store.root())
        .args(["--", "agent", "start", "--data-dir"])
        .arg(&linux_data)
        .env("DOCKER_HOST", "unix:///var/run/docker.sock")
        .env("DOCKER_CONFIG", input.root.join("docker-config"))
        .env("GRIDSTATOR_ACT", "/nonexistent/private-proof-act")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    for key in [
        "GRIDSTATOR_SUPERVISED",
        "GRIDSTATOR_INSTALL_ROOT",
        "GRIDSTATOR_INSTALL_VERSION",
        "GRIDSTATOR_BUILD_ID",
    ] {
        command.env_remove(key);
    }
    let worker = OwnedWorker(command.spawn()?);
    println!(
        "LAN_HANDOFF_READY {}",
        input.root.join("lan-handoff.json").display()
    );
    let workers = named_workers(client, &input.external_worker_names, &baseline.build_id).await?;
    ensure!(
        workers[0]["info"]["build"]["target"] == "linux/amd64"
            && workers[1]["info"]["build"]["target"] == "darwin/arm64",
        "mixed proof did not use two actual distinct targets"
    );
    Ok((worker, workers))
}
async fn roll_workers(input: &Inputs, client: &Client, target: &ReleaseManifest) -> Result<Value> {
    client
        .post("/v1/updates", json!({"nodes":input.external_worker_names}))
        .await?;
    let mut observations = vec![];
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let view = client.get("/v1/updates").await?;
            let entries = view["rollout"]["nodes"]
                .as_array()
                .context("rollout entries")?;
            let active = entries
                .iter()
                .filter(|entry| matches!(entry["state"].as_str(), Some("draining" | "updating")))
                .count();
            ensure!(
                active <= 1,
                "mixed rollout updated more than one worker at once"
            );
            observations.push(view.clone());
            state::write_json(
                &input.root.join("mixed-rollout-progress.json"),
                &json!(observations),
            )?;
            ensure!(
                view["rollout"]["state"] != "halted",
                "positive mixed rollout halted: {view}"
            );
            if view["rollout"]["state"] == "finished" {
                ensure!(
                    entries.len() == 2 && entries.iter().all(|entry| entry["state"] == "done"),
                    "mixed rollout did not complete both workers"
                );
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("mixed rollout deadline")??;
    let workers = named_workers(client, &input.external_worker_names, &target.build_id).await?;
    let mut jobs = vec![];
    for (index, node) in workers.iter().enumerate() {
        ensure!(
            node["settings"]["autoUpdate"] == "off",
            "standalone worker must remain pinned/off for later negative fixture"
        );
        let key = node["info"]["build"]["target"]
            .as_str()
            .context("reported target")?;
        ensure!(
            node["info"]["build"]["sha256"] == target.target(key)?.executable.sha256,
            "mixed worker executable identity differs from actual target artifact"
        );
        let job=client.post("/v1/jobs",json!({"spec":{"kind":"command","image":"alpine:3.21","command":["sh","-c","uname -m; echo mixed-rollout"],"resources":{"cpus":1,"memoryMb":128},"target":{"node":node["id"]}}})).await?;
        let id = job["id"].as_str().context("mixed job ID")?;
        let final_job = wait_job(client, id, "succeeded").await?;
        let page = client.get(&format!("/v1/jobs/{id}/logs")).await?;
        let stdout = page["entries"]
            .as_array()
            .context("mixed logs")?
            .iter()
            .filter(|entry| entry["o"] == "stdout")
            .filter_map(|entry| entry["d"].as_str())
            .collect::<String>();
        ensure!(
            stdout
                == if index == 0 {
                    "x86_64\nmixed-rollout\n"
                } else {
                    "aarch64\nmixed-rollout\n"
                },
            "mixed actual target job output changed: {stdout:?}"
        );
        jobs.push(json!({"job":final_job,"stdout":stdout,"target":key}));
    }
    Ok(
        json!({"workers":workers,"observations":observations,"jobs":jobs,"actualDistinctTargets":["linux/amd64","darwin/arm64"],"positiveArtifacts":"actual real four-target release bundle; no header fixtures"}),
    )
}
async fn embedded(client: &Client, expected: &str) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let nodes = client.get("/v1/nodes").await?;
            if let Some(node) = nodes["nodes"].as_array().and_then(|nodes| {
                nodes.iter().find(|node| {
                    node["name"] == expected
                        && node["status"] == "online"
                        && node["info"]["docker"]["available"] == true
                })
            }) {
                return Ok::<_, anyhow::Error>(node.clone());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("owned embedded Docker worker did not become ready")?
}
async fn wait_job(client: &Client, id: &str, state: &str) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = client.get(&format!("/v1/jobs/{id}")).await?;
            if job["state"] == state {
                return Ok::<_, anyhow::Error>(job);
            }
            ensure!(
                !gridstator::protocol::terminal(job["state"].as_str().unwrap_or("")),
                "owned job ended unexpectedly: {job}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("owned job state deadline")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit root-only authorized host fixture; installs one unique system unit, no reboot"]
async fn actual_root_system_service_drains_and_restores_database_after_schema_crash() -> Result<()>
{
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "requires authorized root host"
    );
    ensure!(
        fs::read_to_string("/proc/1/comm")?.trim() == "systemd",
        "requires actual system manager"
    );
    let input: Inputs = serde_json::from_slice(&fs::read(
        std::env::var_os("GRIDSTATOR_SYSTEM_PROOF_INPUT")
            .context("explicit private fixture input required")?,
    )?)?;
    ensure!(
        input.root.starts_with("/tmp")
            && input.root.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("gridstator-system-proof-")),
        "private proof namespace required"
    );
    ensure!(
        input.unit.starts_with("gridstator-native-systemproof-")
            && !input.unit.ends_with(".service"),
        "unique owned unit name required"
    );
    ensure!(
        !Path::new("/etc/systemd/system")
            .join(format!("{}.service", input.unit))
            .exists(),
        "refusing existing unit"
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    state::private_dir(&input.root)?;
    let before_agent = existing_agent()?;
    ensure!(
        before_agent["state"]
            .as_str()
            .unwrap()
            .contains("ActiveState=active"),
        "existing agent must remain active"
    );
    state::write_json(
        &input.root.join("existing-agent-before.json"),
        &before_agent,
    )?;
    let bytes = fs::read(input.release_directory.join("gridstator-release.json"))?;
    let mut manifest = ReleaseManifest::from_bytes(&bytes)?;
    let baseline_manifest = manifest.clone();
    let target = release::host_target()?;
    let store = InstallStore::new(input.root.join("install"));
    store.install_verified(
        &manifest,
        target,
        input
            .release_directory
            .join(&manifest.target(target)?.archive.file),
    )?;
    store.activate(&manifest.build_id, None)?;
    let executable = store.executable(&manifest.build_id)?;
    let supervisor = store.install_supervisor(&executable, false)?;
    let data = input.root.join("controller");
    let login = input.root.join("client.json");
    let host = input.controller_host.as_deref().unwrap_or("127.0.0.1");
    ensure!(
        host.parse::<std::net::IpAddr>().is_ok(),
        "explicit IP required"
    );
    let port = if input.external_worker_names.is_empty() {
        0
    } else {
        ensure!(
            host != "127.0.0.1",
            "mixed-target proof requires a LAN listener"
        );
        std::net::TcpListener::bind((host, 0))?.local_addr()?.port()
    };
    controller::initialize(&data, host, port, &[], false, &login).await?;
    let original: Config = serde_json::from_slice(&fs::read(&login)?)?;
    let database = db::Db::open(&data.join("controller.db"))?;
    database.write(|connection|db::put(connection,"settings","cluster",&json!({"jobRetentionDays":90,"logRetentionDays":30,"snapshotRetentionDays":7,"autoUpdate":"off","updateWindow":null,"toolchain":{"act":"off"}}))).await?;
    drop(database);
    let mirror = ArtifactMirror::new(data.join("native-release-mirror"));
    mirror.cache_manifest(&bytes, &release::sha256_bytes(&bytes))?;
    for (key, artifact) in &manifest.artifacts {
        mirror.cache_artifact(
            &manifest,
            key,
            input.release_directory.join(&artifact.archive.file),
        )?;
    }
    let mut config = ServiceConfig::new(
        ServiceRole::Controller,
        supervisor,
        store.root().to_path_buf(),
        data.clone(),
        "/root".into(),
        0,
    );
    config.scope = ServiceScope::System;
    config.user = Some("root".into());
    config.name = input.unit.clone();
    config.environment.insert(
        "GRIDSTATOR_CONFIG".into(),
        login.to_string_lossy().into_owned(),
    );
    config.environment.insert(
        "GRIDSTATOR_CACHE".into(),
        input.root.join("cache").to_string_lossy().into_owned(),
    );
    config
        .environment
        .insert("DOCKER_HOST".into(), "unix:///var/run/docker.sock".into());
    config.environment.insert(
        "DOCKER_CONFIG".into(),
        input
            .root
            .join("docker-config")
            .to_string_lossy()
            .into_owned(),
    );
    let embedded_name = format!("{}-worker", input.unit);
    config.extra_args = vec![
        "--with-agent".into(),
        "--agent-name".into(),
        embedded_name.clone(),
    ];
    let service = OwnedService {
        manager: ServiceManager::new(ServicePlatform::Systemd),
        config,
    };
    fs::write(
        input.root.join("owned-unit.service"),
        service.manager.render(&service.config)?,
    )?;
    service.manager.install(&service.config, false)?;
    let (client, pid) = ready(&data, &original, host, None).await?;
    let initial_node = embedded(&client, &embedded_name).await?;
    let cluster = client.get("/v1/cluster").await?;
    let unit = service.config.unit_name();
    let startup = system(&[
        "show",
        &unit,
        "--property=ActiveState",
        "--property=SubState",
        "--property=User",
        "--property=UnitFileState",
        "--property=MainPID",
        "--property=InvocationID",
        "--no-pager",
    ])?;
    ensure!(
        startup.contains("User=root")
            && startup.contains("ActiveState=active")
            && startup.contains("UnitFileState=enabled"),
        "system unit not active/enabled/root: {startup}"
    );
    let job=client.post("/v1/jobs",json!({"spec":{"kind":"command","image":"alpine:3.21","command":["echo","system-root-marker"],"resources":{"cpus":1,"memoryMb":64}}})).await?;
    let marker = job["id"].as_str().context("marker job ID")?;
    wait_job(&client, marker, "succeeded").await?;
    service
        .manager
        .control(&service.config, ServiceAction::Restart)?;
    let (mut client, mut restarted) = ready(&data, &original, host, Some(pid)).await?;
    ensure!(pid != restarted, "systemctl restart retained old payload");
    embedded(&client, &embedded_name).await?;
    ensure!(
        client.get(&format!("/v1/jobs/{marker}")).await?["state"] == "succeeded",
        "system restart lost job history"
    );
    let mut standalone = None;
    let mut mixed = Value::Null;
    let mut initial_external = vec![];
    let mut positive_controller = Value::Null;
    if !input.external_worker_names.is_empty() {
        let (worker, nodes) = start_lan_workers(&input, &client, &baseline_manifest).await?;
        standalone = Some(worker);
        initial_external = nodes;
        let positive_directory = input
            .positive_release_directory
            .as_ref()
            .context("actual positive release directory required for mixed proof")?;
        let positive = ReleaseManifest::read(positive_directory.join("gridstator-release.json"))?;
        ensure!(
            positive.build_id != baseline_manifest.build_id,
            "positive update must use a distinct real build ID"
        );
        let published = publish_bundle(
            &client,
            input
                .positive_bundle
                .as_ref()
                .context("actual four-target positive bundle required")?,
        )
        .await?;
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                if store.state()?.is_some_and(|state| {
                    state.current == positive.build_id && state.pending.is_none()
                }) {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("actual positive controller activation not confirmed")??;
        let old_pid = restarted;
        (client, restarted) = ready(&data, &original, host, Some(old_pid)).await?;
        embedded(&client, &embedded_name).await?;
        ensure!(
            client.get("/v1/cluster").await?["clusterId"] == cluster["clusterId"],
            "positive update changed cluster identity"
        );
        named_workers(
            &client,
            &input.external_worker_names,
            &baseline_manifest.build_id,
        )
        .await?;
        mixed = roll_workers(&input, &client, &positive).await?;
        manifest = positive;
        let worker_store = InstallStore::new(input.root.join("linux-install"));
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if worker_store.state()?.is_some_and(|state| {
                    state.current == manifest.build_id && state.pending.is_none()
                }) {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("Linux worker pending confirmation did not clear")??;
        manifest
            .target(target)?
            .executable
            .verify(store.supervisor_path())?;
        let positive_pid = restarted;
        service
            .manager
            .control(&service.config, ServiceAction::Restart)?;
        (client, restarted) = ready(&data, &original, host, Some(positive_pid)).await?;
        named_workers(&client, &input.external_worker_names, &manifest.build_id).await?;
        embedded(&client, &embedded_name).await?;
        positive_controller = json!({"from":baseline_manifest.build_id,"to":manifest.build_id,"oldPayloadPid":old_pid,"confirmedPayloadPid":positive_pid,"restartedFinalPayloadPid":restarted,"published":published,"stableSupervisorMatchesFinal":true,"linuxWorkerInstall":worker_store.state()?});
        state::write_json(
            &input.root.join("mixed-positive-finished.json"),
            &json!({"controller":positive_controller,"mixed":mixed}),
        )?;
        println!(
            "MIXED_POSITIVE_DONE {}",
            input.root.join("mixed-positive-finished.json").display()
        );
    }
    let drain=client.post("/v1/jobs",json!({"spec":{"kind":"command","image":"alpine:3.21","command":["sh","-c","echo system-scope-drain; sleep 12"],"resources":{"cpus":1,"memoryMb":64},"target":{"node":initial_node["id"]}}})).await?;
    let drain_id = drain["id"].as_str().context("drain job ID")?;
    wait_job(&client, drain_id, "running").await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let page = client.get(&format!("/v1/jobs/{drain_id}/logs")).await?;
            if page["entries"].as_array().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry["d"]
                        .as_str()
                        .is_some_and(|text| text.contains("system-scope-drain"))
                })
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let main_before = system(&["show", &unit, "--property=MainPID", "--value"])?;
    let before = facts(&data.join("controller.db"))?;
    let (bundle_size, bundle_sha) = release::hash_file(&input.bad_bundle)?;
    let response = client
        .http
        .post(format!(
            "{}/v1/controller/build?sha256={bundle_sha}",
            client.config.url
        ))
        .bearer_auth(&client.config.token)
        .header("content-type", "application/gzip")
        .body(fs::read(&input.bad_bundle)?)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "controlled bad candidate rejected: {}",
        response.text().await?
    );
    let draining = client.get("/v1/updates").await?;
    ensure!(
        draining["controllerUpdate"]["phase"] == "draining",
        "active job was not held in controller drain: {draining}"
    );
    // The old endpoint may close immediately after acknowledging the drained job. Its restored
    // terminal state below proves completion without racing that deliberately closing listener.
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if store.state()?.is_some_and(|state| {
                state.current == manifest.build_id
                    && state.pending.is_none()
                    && state
                        .last_rollback
                        .is_some_and(|rollback| rollback.from == input.bad_build_id)
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("controlled candidate did not roll back")??;
    let (client, restored_pid) = ready(&data, &original, host, Some(restarted)).await?;
    let restored_node = embedded(&client, &embedded_name).await?;
    ensure!(
        client.get("/v1/cluster").await?["clusterId"] == cluster["clusterId"],
        "rollback changed cluster identity"
    );
    ensure!(
        client.get(&format!("/v1/jobs/{marker}")).await?["state"] == "succeeded"
            && client.get(&format!("/v1/jobs/{drain_id}")).await?["state"] == "succeeded",
        "rollback lost acknowledged history"
    );
    let restored = facts(&data.join("controller.db"))?;
    ensure!(
        restored == before,
        "schema/history not restored: {before} -> {restored}"
    );
    let failed = fs::read_dir(&data)?
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("controller.db.failed-{}-", input.bad_build_id))
                && !entry.file_name().to_string_lossy().ends_with("-wal")
                && !entry.file_name().to_string_lossy().ends_with("-shm")
        })
        .context("failed database not retained")?
        .path();
    let failed_facts = facts(&failed)?;
    ensure!(
        failed_facts["userVersion"] == 999
            && failed_facts["candidateTable"] == true
            && failed_facts["candidateColumn"] == true
            && failed_facts["jobs"] == 0,
        "candidate did not actually change schema/history: {failed_facts}"
    );
    ensure!(
        fs::read_to_string(data.join("candidate-mutated-schema"))? == input.bad_build_id,
        "actual schema-fault executable did not run"
    );
    let main_after = system(&["show", &unit, "--property=MainPID", "--value"])?;
    ensure!(
        main_after == main_before,
        "managed rollback restarted systemd supervisor"
    );
    let updates = client.get("/v1/updates").await?;
    if !input.external_worker_names.is_empty() {
        let final_workers =
            named_workers(&client, &input.external_worker_names, &manifest.build_id).await?;
        ensure!(
            final_workers
                .iter()
                .all(|node| node["settings"]["autoUpdate"] == "off"),
            "negative fixture must not offer updates to standalone workers"
        );
        for job in mixed["jobs"]
            .as_array()
            .context("positive mixed job records")?
        {
            let id = job["job"]["id"].as_str().context("mixed history job ID")?;
            ensure!(
                client.get(&format!("/v1/jobs/{id}")).await?["state"] == "succeeded",
                "schema rollback lost mixed-target job history"
            );
        }
        state::write_json(
            &input.root.join("mixed-negative-finished.json"),
            &json!({"buildId":manifest.build_id,"workers":final_workers,"rollback":store.state()?.context("install state")?.last_rollback,"before":before,"restored":restored,"failedDatabase":failed_facts}),
        )?;
        println!(
            "MIXED_NEGATIVE_DONE {}",
            input.root.join("mixed-negative-finished.json").display()
        );
        tokio::time::timeout(Duration::from_secs(600), async {
            while !input.root.join("external-cleanup-complete").exists() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("Mac owner did not confirm private worker cleanup")?;
    }
    if let Some(worker) = standalone.as_mut() {
        worker.stop()?;
    }
    service
        .manager
        .control(&service.config, ServiceAction::Stop)?;
    ensure!(
        system(&["show", &unit, "--property=ActiveState", "--value"])?.trim() == "inactive",
        "owned service did not stop cleanly"
    );
    ensure!(
        !data.join("runtime.json").exists(),
        "owned runtime identity remained after stop"
    );
    let journal = Command::new("journalctl")
        .args(["-u", &unit, "--no-pager", "--output=short-iso"])
        .output()?;
    fs::write(input.root.join("owned-journal.log"), journal.stdout)?;
    drop(service);
    ensure!(
        !Path::new("/etc/systemd/system").join(&unit).exists(),
        "owned system unit remained installed"
    );
    let mut owned_nodes = vec![initial_node.clone()];
    owned_nodes.extend(initial_external.clone());
    for owned_node in &owned_nodes {
        for kind in ["containers", "volumes"] {
            let mut command = Command::new("docker");
            if kind == "containers" {
                command.args(["ps", "-aq"]);
            } else {
                command.args(["volume", "ls", "-q"]);
            }
            let result = command
                .args([
                    "--filter",
                    &format!(
                        "label=io.gridstator.node={}",
                        owned_node["id"].as_str().unwrap()
                    ),
                ])
                .output()?;
            ensure!(
                result.status.success()
                    && String::from_utf8_lossy(&result.stdout).trim().is_empty(),
                "owned Docker {kind} remained after stop"
            );
        }
    }
    let after_agent = existing_agent()?;
    ensure!(
        after_agent == before_agent,
        "existing agent state/bytes changed during isolated proof"
    );
    let report = json!({"scope":"Actual privileged Linux systemd service on authorized UbuntuKVM host; optional actual managed Linux/M4LAN update plus schema rollback; no reboot and no WSL claim","unit":unit,"baselineBuildId":baseline_manifest.build_id,"buildId":manifest.build_id,"releaseManifest":manifest,"clusterId":cluster["clusterId"],"caFingerprint":original.ca_fingerprint,"initialPayloadPid":pid,"restartedPayloadPid":restarted,"restoredPayloadPid":restored_pid,"stableSupervisorPid":main_after.trim(),"startup":startup,"markerJob":marker,"drainJob":drain_id,"draining":draining,"positiveController":positive_controller,"initialExternalWorkers":initial_external,"mixedTargetRollout":mixed,"negativeFixture":{"buildId":input.bad_build_id,"bundleSha256":bundle_sha,"bundleSize":bundle_size,"scope":"actual Linux schema-fault executable; non-host members are header fixtures and workers remain pinned/off"},"before":before,"restored":restored,"failedDatabase":failed_facts,"rollback":store.state()?.context("install state")?.last_rollback,"updates":updates,"initialNode":initial_node,"restoredNode":restored_node,"existingAgentBefore":before_agent,"existingAgentAfter":after_agent,"cleanup":{"uniqueUnitRemoved":true,"ownedDockerResourcesAbsent":true,"runtimeIdentityRemoved":true,"existingAgentUnchanged":true,"externalMacCleanupConfirmed":!input.external_worker_names.is_empty(),"rebootPerformed":false}});
    state::write_json(&input.root.join("proof.json"), &report)?;
    println!(
        "Actual unique root system-service startup/job/restart/drain/schema rollback passed; existing agent unchanged"
    );
    Ok(())
}
