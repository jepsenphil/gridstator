use anyhow::{Context, Result};
use gridstator::{
    client::{Client, Config},
    controller, state,
    worker::{self, LogSpool, SecretMasker, WorkerExit, WorkerOptions},
    workspace::{self, BlobStore, CaptureOptions},
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct Server {
    _temp: TempDir,
    dir: PathBuf,
    client: Client,
    child: tokio::process::Child,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
async fn boot() -> Result<Server> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = TempDir::new()?;
    let dir = temp.path().join("controller");
    let initial = controller::initialize(
        &dir,
        "127.0.0.1",
        0,
        &[],
        false,
        &temp.path().join("client.json"),
    )
    .await?;
    let log = std::fs::File::create(temp.path().join("controller.log"))?;
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .args(["controller", "start", "--data-dir"])
        .arg(&dir)
        .env("GRIDSTATOR_CONFIG", temp.path().join("client.json"))
        .env_remove("GRIDSTATOR_SUPERVISED")
        .env_remove("GRIDSTATOR_INSTALL_ROOT")
        .env_remove("GRIDSTATOR_INSTALL_VERSION")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let pid = child.id();
    let runtime = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(runtime) = state::read_json(&dir.join("runtime.json"))
                && runtime["pid"].as_u64() == pid.map(u64::from)
            {
                break Ok::<_, anyhow::Error>(runtime);
            }
            if let Some(exit) = child.try_wait()? {
                anyhow::bail!("controller exited {exit}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let client = Client::new(Config {
        version: 1,
        url: format!(
            "https://127.0.0.1:{}",
            runtime["port"].as_u64().context("controller port")?
        ),
        ca_pem: std::fs::read_to_string(dir.join("pki/ca.pem"))?,
        ca_fingerprint: initial["caFingerprint"].as_str().unwrap().into(),
        token: initial["token"].as_str().unwrap().into(),
        token_name: Some("local-admin".into()),
    })?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client.get("/v1/health").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    Ok(Server {
        _temp: temp,
        dir,
        client,
        child,
    })
}
async fn pair(server: &Server, dir: &Path) -> Result<String> {
    let token = server
        .client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"worker-contract","uses":1}),
        )
        .await?;
    let joined = worker::join(
        dir,
        &server.client.config.url,
        token["token"].as_str().unwrap(),
        &server.client.config.ca_fingerprint,
        Some(format!("contract-{}", state::id())),
        Some(json!({"reserved":{"cpus":0,"memoryMb":0},"maxConcurrency":2})),
    )
    .await?;
    Ok(joined["nodeId"].as_str().unwrap().into())
}
async fn online(server: &Server, node: &str) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let nodes = server.client.get("/v1/nodes").await?;
            if let Some(node) = nodes["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|candidate| candidate["id"] == node && candidate["status"] == "online")
            {
                return Ok::<_, anyhow::Error>(node.clone());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?
}
fn spec(node: &str, command: &str) -> Value {
    json!({"kind":"command","image":"alpine:3.20","command":["sh","-c",command],"target":{"node":node},"resources":{"cpus":1,"memoryMb":128},"timeoutSeconds":30})
}
async fn submit(server: &Server, spec: Value) -> Result<String> {
    Ok(
        server.client.post("/v1/jobs", json!({"spec":spec})).await?["id"]
            .as_str()
            .context("job ID")?
            .into(),
    )
}
async fn wait_job(server: &Server, id: &str, state: &str) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let job = server.client.get(&format!("/v1/jobs/{id}")).await?;
            if job["state"] == state {
                return Ok::<_, anyhow::Error>(job);
            }
            if gridstator::protocol::terminal(job["state"].as_str().unwrap_or("")) {
                anyhow::bail!("job {id} ended as {}: {}", job["state"], job["error"]);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await?
}
fn options(drain: bool) -> WorkerOptions {
    WorkerOptions {
        drain,
        heartbeat_interval: Duration::from_millis(100),
        reprobe_interval: Duration::from_secs(1),
        reconnect_min: Duration::from_millis(100),
        reconnect_max: Duration::from_millis(500),
        shutdown_result_grace: Duration::from_secs(1),
        ..Default::default()
    }
}
fn require_docker() {
    let result = std::process::Command::new("docker")
        .arg("version")
        .output()
        .expect("Docker required for worker integration tests");
    assert!(result.status.success());
}

#[test]
fn secret_masking_preserves_utf8_across_chunks_and_separates_streams() -> Result<()> {
    let mut masker = SecretMasker::new(vec!["native-💾-secret".into(), "line1\nline2".into()])?;
    let mut out = masker.push("stdout", "before native-");
    out.push_str(&masker.push("stdout", "💾-"));
    out.push_str(&masker.push("stdout", "secret after\n"));
    assert_eq!(out, "before *** after\n");
    assert_eq!(masker.push("stdout", "line1\n"), "");
    assert_eq!(masker.push("stderr", "ordinary"), "ordinary");
    assert_eq!(masker.push("stdout", "line2!"), "***!");
    assert_eq!(masker.push("stdout", "native-"), "");
    assert_eq!(masker.finish(), [("stdout".into(), "native-".into())]);
    Ok(())
}
#[test]
fn spool_is_durable_sequenced_masked_and_bounded_after_truncation() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("logs.ndjson");
    let mut spool = LogSpool::open(&path, vec!["secret-value".into()], 128)?;
    spool.push("stdout", "prefix secret-")?;
    spool.push("stdout", "value suffix\n")?;
    spool.push("stderr", &"x".repeat(1024))?;
    spool.push("stdout", "discarded")?;
    spool.finish()?;
    drop(spool);
    let entries = worker::replay_spool(&path, 0, 100)?;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["s"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (1..=entries.len() as u64).collect::<Vec<_>>()
    );
    let text = entries
        .iter()
        .filter_map(|entry| entry["d"].as_str())
        .collect::<String>();
    assert!(text.contains("***"));
    assert!(!text.contains("secret-value"));
    assert!(text.contains("Output truncated"));
    assert!(!text.contains("discarded"));
    assert!(std::fs::metadata(path)?.len() < 2048);
    Ok(())
}
#[test]
fn process_recovery_preserves_torn_log_high_water_and_existing_results() -> Result<()> {
    let temp = TempDir::new()?;
    let id = state::id();
    let job = temp.path().join("jobs").join(&id);
    state::private_dir(&job)?;
    state::write_json(
        &job.join("assignment.json"),
        &json!({"jobId":id,"assignmentId":"assignment"}),
    )?;
    let mut spool = LogSpool::open(&job.join("logs.ndjson"), vec![], 1024)?;
    spool.push("stdout", "before crash\n")?;
    drop(spool);
    std::fs::OpenOptions::new()
        .append(true)
        .open(job.join("logs.ndjson"))?
        .write_all(b"{\"s\":2")?;
    worker::recover_interrupted(temp.path())?;
    let result = state::read_json(&job.join("result.json"))?;
    assert_eq!(result["outcome"], "lost");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("restarted or lost track")
    );
    assert_eq!(result["logSeq"], 1);
    let mut completed = result;
    completed["outcome"] = json!("succeeded");
    state::write_json(&job.join("result.json"), &completed)?;
    worker::recover_interrupted(temp.path())?;
    assert_eq!(
        state::read_json(&job.join("result.json"))?["outcome"],
        "succeeded"
    );
    Ok(())
}
#[tokio::test]
async fn pinned_relocation_persists_before_retarget_and_rejects_other_cluster() -> Result<()> {
    let server = boot().await?;
    let foreign = boot().await?;
    let data = TempDir::new()?;
    let mut config = json!({"version":2,"controllerUrl":"https://127.0.0.1:1","controllerFallbackUrls":[],"caPem":server.client.config.ca_pem,"caFingerprint":server.client.config.ca_fingerprint,"nodeId":"worker","name":"worker","credential":server.client.config.token});
    state::write_json(&data.path().join("agent.json"), &config)?;
    let mut initial = server.client.config.clone();
    initial.url = "https://127.0.0.1:1".into();
    let mut client = Client::new(initial)?;
    assert!(
        worker::relocate_controller(
            data.path(),
            &mut config,
            &mut client,
            &server.client.config.url
        )
        .await?
    );
    assert_eq!(client.config.url, server.client.config.url);
    assert_eq!(client.get("/v1/health").await?["ok"], true);
    assert_eq!(config["controllerFallbackUrls"][0], "https://127.0.0.1:1");
    assert_eq!(state::read_json(&data.path().join("agent.json"))?, config);
    let before = config.clone();
    assert!(
        worker::relocate_controller(
            data.path(),
            &mut config,
            &mut client,
            &foreign.client.config.url
        )
        .await
        .is_err()
    );
    assert_eq!(config, before);
    assert_eq!(client.config.url, server.client.config.url);
    assert_eq!(state::read_json(&data.path().join("agent.json"))?, before);
    Ok(())
}
#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn drain_keeps_heartbeat_and_finishes_running_job_without_accepting_more() -> Result<()> {
    require_docker();
    let server = boot().await.context("drain: controller startup")?;
    let data = TempDir::new()?;
    let node = pair(&server, data.path())
        .await
        .context("drain: worker pairing")?;
    let stop = CancellationToken::new();
    let actor = tokio::spawn(worker::run_with_shutdown(
        data.path().into(),
        options(true),
        stop.clone(),
        None,
    ));
    online(&server, &node)
        .await
        .context("drain: worker online")?;
    let duplicate = worker::run_with_shutdown(
        data.path().into(),
        options(false),
        CancellationToken::new(),
        None,
    )
    .await;
    assert!(
        duplicate
            .unwrap_err()
            .to_string()
            .contains("another worker owns this data directory")
    );
    let id = submit(
        &server,
        spec(&node, "echo drain-started; sleep 2; echo drain-complete"),
    )
    .await
    .context("drain: submit initial job")?;
    wait_job(&server, &id, "running")
        .await
        .context("drain: initial job running")?;
    stop.cancel();
    let later = submit(&server, spec(&node, "echo should-not-run-during-drain"))
        .await
        .context("drain: submit job after stop")?;
    tokio::time::sleep(Duration::from_millis(450)).await;
    let nodes = server.client.get("/v1/nodes").await?;
    let current = nodes["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["id"] == node)
        .unwrap();
    assert_eq!(current["status"], "online");
    assert!(current["lastSeenAt"].as_i64().unwrap() > state::now() - 1000);
    wait_job(&server, &id, "succeeded")
        .await
        .context("drain: active job succeeded")?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), actor)
            .await
            .context("drain: worker exit after result acknowledgment")???,
        WorkerExit::Stopped
    );
    let later = server.client.get(&format!("/v1/jobs/{later}")).await?;
    assert!(!gridstator::protocol::terminal(
        later["state"].as_str().unwrap()
    ));
    let logs = server.client.get(&format!("/v1/jobs/{id}/logs")).await?;
    assert!(
        logs["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["d"].as_str().unwrap_or("").contains("drain-complete"))
    );
    Ok(())
}
#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn ordinary_shutdown_stops_work_and_persists_lost_result() -> Result<()> {
    require_docker();
    let server = boot().await?;
    let data = TempDir::new()?;
    let node = pair(&server, data.path()).await?;
    let stop = CancellationToken::new();
    let actor = tokio::spawn(worker::run_with_shutdown(
        data.path().into(),
        options(false),
        stop.clone(),
        None,
    ));
    online(&server, &node).await?;
    let id = submit(&server, spec(&node, "echo shutdown-started; exec sleep 30")).await?;
    wait_job(&server, &id, "running").await?;
    stop.cancel();
    let job = wait_job(&server, &id, "lost").await?;
    assert!(job["error"].as_str().unwrap_or("").contains("shut down"));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(15), actor).await???,
        WorkerExit::Stopped
    );
    Ok(())
}
#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn missing_snapshot_blob_yields_terminal_result_instead_of_stranding_job() -> Result<()> {
    require_docker();
    let server = boot().await?;
    let data = TempDir::new()?;
    let source = TempDir::new()?;
    std::fs::write(source.path().join("source.txt"), "submitted bytes")?;
    let capture = Arc::new(workspace::capture(
        source.path(),
        CaptureOptions::default(),
    )?);
    gridstator::transfer::upload_capture(&server.client, capture.clone()).await?;
    let store = BlobStore::new(server.dir.join("blobs"));
    let digest = capture.blobs.keys().next().unwrap();
    std::fs::remove_file(store.path(digest)?)?;
    let node = pair(&server, data.path()).await?;
    let stop = CancellationToken::new();
    let actor = tokio::spawn(worker::run_with_shutdown(
        data.path().into(),
        options(false),
        stop.clone(),
        None,
    ));
    online(&server, &node).await?;
    let mut request = spec(&node, "cat source.txt");
    request["snapshot"] = json!(capture.id);
    let id = submit(&server, request).await?;
    let job = wait_job(&server, &id, "failed").await?;
    assert!(!job["error"].is_null());
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), actor).await???;
    Ok(())
}
#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn artifact_upload_failure_still_delivers_durable_command_result() -> Result<()> {
    require_docker();
    let server = boot().await?;
    let data = TempDir::new()?;
    let node = pair(&server, data.path()).await?;
    let stop = CancellationToken::new();
    let actor = tokio::spawn(worker::run_with_shutdown(
        data.path().into(),
        options(false),
        stop.clone(),
        None,
    ));
    online(&server, &node).await?;
    let blob_root = server.dir.join("blobs");
    if blob_root.is_dir() {
        std::fs::remove_dir_all(&blob_root)?;
    }
    std::fs::write(&blob_root, "blocked store")?;
    let mut request = spec(&node, "echo report > report.txt");
    request["artifacts"] = json!(["report.txt"]);
    let id = submit(&server, request).await?;
    let job = wait_job(&server, &id, "succeeded").await?;
    assert!(job["artifacts"].as_array().unwrap().is_empty());
    let logs = server.client.get(&format!("/v1/jobs/{id}/logs")).await?;
    assert!(
        logs["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["d"].as_str().unwrap_or("").contains("upload failed"))
    );
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), actor).await???;
    Ok(())
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn disconnect_through_completion_replays_durable_logs_before_result_ack() -> Result<()> {
    require_docker();
    let mut server = boot().await?;
    let data = TempDir::new()?;
    let node = pair(&server, data.path()).await?;
    let stop = CancellationToken::new();
    let actor = tokio::spawn(worker::run_with_shutdown(
        data.path().into(),
        options(false),
        stop.clone(),
        None,
    ));
    online(&server, &node).await?;
    let id = submit(
        &server,
        spec(
            &node,
            "echo replay-1; sleep 1; printf 'replay-2 \\360\\237'; sleep 0.05; printf '\\246\\200\\n'; sleep 1; printf 'replay-3 \\342\\202\\254'",
        ),
    )
    .await?;
    wait_job(&server, &id, "running").await?;
    let config_path = server.dir.join("controller.json");
    let mut config = state::read_json(&config_path)?;
    config["port"] = json!(url::Url::parse(&server.client.config.url)?.port().unwrap());
    state::write_json(&config_path, &config)?;
    server.child.start_kill()?;
    server.child.wait().await?;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if data
                .path()
                .join("jobs")
                .join(&id)
                .join("result.json")
                .is_file()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await?;
    let result = state::read_json(&data.path().join("jobs").join(&id).join("result.json"))?;
    assert_eq!(result["outcome"], "succeeded");
    let log = std::fs::File::create(server._temp.path().join("controller-restart.log"))?;
    server.child = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"))
        .args(["controller", "start", "--data-dir"])
        .arg(&server.dir)
        .env("GRIDSTATOR_CONFIG", server._temp.path().join("client.json"))
        .env_remove("GRIDSTATOR_SUPERVISED")
        .env_remove("GRIDSTATOR_INSTALL_ROOT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if server.client.get("/v1/health").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await?;
    wait_job(&server, &id, "succeeded").await?;
    let logs = server.client.get(&format!("/v1/jobs/{id}/logs")).await?;
    let entries = logs["entries"].as_array().unwrap();
    let text = entries
        .iter()
        .filter_map(|entry| entry["d"].as_str())
        .collect::<String>();
    for line in ["replay-1", "replay-2", "replay-3"] {
        assert_eq!(text.matches(line).count(), 1, "{text}");
    }
    let stdout = entries
        .iter()
        .filter(|entry| entry["o"] == "stdout")
        .filter_map(|entry| entry["d"].as_str())
        .collect::<String>();
    assert_eq!(
        stdout, "replay-1\nreplay-2 🦀\nreplay-3 €",
        "UTF8 chunks and the final partial line changed during durable replay"
    );
    assert_eq!(entries.last().unwrap()["s"], result["logSeq"]);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), actor).await???;
    assert!(
        !data.path().join("jobs").join(id).exists(),
        "ack removes only the completed durable outbox"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn actual_agent_cli_sigterm_drain_finishes_work_and_exits_cleanly() -> Result<()> {
    require_docker();
    let server = boot().await?;
    let data = TempDir::new()?;
    let source = TempDir::new()?;
    std::fs::write(source.path().join("large.txt"), vec![b'x'; 2 * 1024 * 1024])?;
    let captured = Arc::new(workspace::capture(
        source.path(),
        CaptureOptions::default(),
    )?);
    gridstator::transfer::upload_capture(&server.client, captured.clone()).await?;
    let node = pair(&server, data.path()).await?;
    let log = std::fs::File::create(data.path().join("agent-process.log"))?;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"))
        .args(["agent", "start", "--data-dir"])
        .arg(data.path())
        .args(["--drain", "--cache-size", "1m"])
        .env_remove("GRIDSTATOR_SUPERVISED")
        .env_remove("GRIDSTATOR_INSTALL_ROOT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()?;
    online(&server, &node).await?;
    let mut request = spec(
        &node,
        "wc -c large.txt; echo cli-drain-start; sleep 2; echo cli-drain-end",
    );
    request["snapshot"] = json!(captured.id);
    let id = submit(&server, request).await?;
    wait_job(&server, &id, "running").await?;
    let pid = child.id().context("agent PID missing")?;
    // SAFETY: the positive PID is a live, owned test child; no process group is targeted.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    wait_job(&server, &id, "succeeded").await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(15), child.wait())
            .await??
            .success()
    );
    assert!(
        !BlobStore::new(data.path().join("blobs")).has(captured.blobs.keys().next().unwrap())?,
        "a blob larger than the configured cache must be evicted after the active snapshot is unpinned"
    );
    let logs = server.client.get(&format!("/v1/jobs/{id}/logs")).await?;
    assert!(
        logs["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["d"].as_str().unwrap_or("").contains("cli-drain-end"))
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn update_probe_releases_admission_and_restart_handoff_is_explicit() -> Result<()> {
    require_docker();
    let server = boot().await?;
    let data = TempDir::new()?;
    let node = pair(&server, data.path()).await?;
    let stop = CancellationToken::new();
    let checking = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook: worker::UpdateHook = Arc::new({
        let checking = checking.clone();
        let release = release.clone();
        move |check| {
            let checking = checking.clone();
            let release = release.clone();
            let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if call == 1 {
                    assert_eq!(check.active_jobs, 0);
                    assert_eq!(check.pending_results, 0);
                    checking.notify_one();
                    tokio::select! { _=release.notified()=>{}, _=check.cancel.cancelled()=>{} }
                }
                Ok(worker::UpdateDecision::None)
            })
        }
    });
    let mut worker_options = options(false);
    worker_options.update_interval = Duration::from_secs(1);
    worker_options.reprobe_interval = Duration::from_secs(3600);
    let actor = tokio::spawn(worker::run_with_shutdown(
        data.path().into(),
        worker_options,
        stop.clone(),
        Some(hook),
    ));
    online(&server, &node)
        .await
        .context("worker online before update probe")?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let nodes = server.client.get("/v1/nodes").await?;
            if nodes["nodes"].as_array().unwrap().iter().any(|candidate| {
                candidate["id"] == node
                    && candidate["info"]["executors"]
                        .as_array()
                        .is_some_and(|executors| executors.contains(&json!("command")))
            }) {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("worker admission restored after update probe")??;
    tokio::time::timeout(Duration::from_secs(10), checking.notified())
        .await
        .context("periodic update admission hold")?;
    let id = submit(&server, spec(&node, "echo after-update-probe")).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = server.client.get(&format!("/v1/jobs/{id}")).await?;
            if job["state"] == "queued"
                && job["stateReason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("rejected"))
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("assignment rejected during a periodic update check")??;
    release.notify_one();
    wait_job(&server, &id, "succeeded")
        .await
        .context("job after update probe")?;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), actor)
        .await
        .context("worker shutdown after update probe")???;
    let restart: worker::UpdateHook =
        Arc::new(|_| Box::pin(async { Ok(worker::UpdateDecision::Restart) }));
    let exit = tokio::time::timeout(
        Duration::from_secs(15),
        worker::run_with_shutdown(
            data.path().into(),
            options(false),
            CancellationToken::new(),
            Some(restart),
        ),
    )
    .await
    .context("worker restart handoff after update probe")??;
    assert_eq!(exit, WorkerExit::Restart);
    Ok(())
}

#[tokio::test]
async fn shutdown_retains_worker_ownership_until_started_update_finishes() -> Result<()> {
    struct ReleaseStage(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseStage {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let server = boot().await?;
    for abort in [false, true] {
        let data = TempDir::new()?;
        pair(&server, data.path()).await?;
        let (started_send, started_receive) = tokio::sync::oneshot::channel();
        let (release_send, release_receive) = std::sync::mpsc::channel();
        // A panic must release the controlled blocking task so the test runtime can shut down.
        let mut release = ReleaseStage(Some(release_send));
        let stage = Arc::new(Mutex::new(Some((started_send, release_receive))));
        let hook: worker::UpdateHook = Arc::new(move |check| {
            let stage = stage.lock().unwrap().take();
            Box::pin(async move {
                if let Some((started, release)) = stage {
                    tokio::task::spawn_blocking(move || -> Result<()> {
                        let _ = started.send(());
                        release.recv_timeout(Duration::from_secs(15))?;
                        std::fs::write(check.dir.join("update-stage-finished"), b"finished")?;
                        Ok(())
                    })
                    .await??;
                }
                Ok(worker::UpdateDecision::None)
            })
        });
        let stop = CancellationToken::new();
        let mut actor = tokio::spawn(worker::run_with_shutdown(
            data.path().into(),
            options(false),
            stop.clone(),
            Some(hook),
        ));
        tokio::time::timeout(Duration::from_secs(15), started_receive).await??;
        let instance = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(data.path().join("agent.lock"))?;
        if abort {
            actor.abort();
            assert!((&mut actor).await.unwrap_err().is_cancelled());
        } else {
            stop.cancel();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), &mut actor)
                    .await
                    .is_err(),
                "shutdown returned while its non-cancellable update stage was running"
            );
        }
        assert_eq!(
            fs2::FileExt::try_lock_exclusive(&instance)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock,
            "a replacement worker cannot enter during an owned update mutation"
        );
        release.0.take().unwrap().send(())?;
        if !abort {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(10), &mut actor).await???,
                WorkerExit::Stopped
            );
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while fs2::FileExt::try_lock_exclusive(&instance).is_err() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        assert_eq!(
            std::fs::read(data.path().join("update-stage-finished"))?,
            b"finished"
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn aborting_the_coordinator_cancels_owned_work_and_preserves_lost_outbox() -> Result<()> {
    require_docker();
    let server = boot().await?;
    let data = TempDir::new()?;
    let node = pair(&server, data.path()).await?;
    let actor = tokio::spawn(worker::run_with_shutdown(
        data.path().into(),
        options(false),
        CancellationToken::new(),
        None,
    ));
    online(&server, &node).await?;
    let id = submit(&server, spec(&node, "echo abort-started; exec sleep 30")).await?;
    wait_job(&server, &id, "running").await?;
    actor.abort();
    let _ = actor.await;
    let path = data.path().join("jobs").join(&id).join("result.json");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if path.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    assert_eq!(state::read_json(&path)?["outcome"], "lost");
    let containers = std::process::Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=io.gridstator.job={id}"),
        ])
        .output()?;
    assert!(containers.status.success());
    assert!(
        containers.stdout.is_empty(),
        "an aborted socket actor must not abandon a running Docker workload"
    );
    Ok(())
}

#[cfg(unix)]
struct OwnedUpdateRole(std::process::Child);
#[cfg(unix)]
impl OwnedUpdateRole {
    fn launch(
        binary: &Path,
        args: &[std::ffi::OsString],
        log: &Path,
        config: &Path,
    ) -> Result<Self> {
        let output = std::fs::File::create(log)?;
        Ok(Self(
            std::process::Command::new(binary)
                .args(args)
                .env("GRIDSTATOR_CONFIG", config)
                .env(
                    "GRIDSTATOR_DOCKER",
                    "/nonexistent/gridstator-contract-docker",
                )
                .env("GRIDSTATOR_ACT", "/nonexistent/gridstator-contract-act")
                .env_remove("GRIDSTATOR_SUPERVISED")
                .env_remove("GRIDSTATOR_INSTALL_ROOT")
                .env_remove("GRIDSTATOR_INSTALL_VERSION")
                .stdin(Stdio::null())
                .stdout(output.try_clone()?)
                .stderr(output)
                .spawn()?,
        ))
    }
}
#[cfg(unix)]
impl Drop for OwnedUpdateRole {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            // A test may deliberately pause the owned launcher at the activation boundary.
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGCONT);
            }
            // SAFETY: signal only the owned supervisor/controller PID. Its ordinary shutdown
            // performs the production payload-session cleanup before exiting.
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            for _ in 0..100 {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(unix)]
fn managed_update_payload_pid(supervisor: u32, expected: &Path) -> Result<u32> {
    let expected = std::fs::canonicalize(expected)?;
    let system = sysinfo::System::new_all();
    system
        .processes()
        .values()
        .find(|process| {
            process.parent() == Some(sysinfo::Pid::from_u32(supervisor))
                && process.exe().is_some_and(|path| {
                    std::fs::canonicalize(path).is_ok_and(|path| path == expected)
                })
        })
        .map(|process| process.pid().as_u32())
        .context("owned native supervisor payload PID")
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires two real native release packages; set GRIDSTATOR_WORKER_UPDATE_FIXTURES to the recovery rehearsal directory"]
async fn independent_managed_worker_installs_restarts_rejoins_and_unmanaged_worker_stays_put()
-> Result<()> {
    use gridstator::{
        install::InstallStore,
        release::{self, ArtifactMirror, ReleaseManifest},
    };
    use std::ffi::OsString;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let packages = PathBuf::from(
        std::env::var_os("GRIDSTATOR_WORKER_UPDATE_FIXTURES").context(
            "set GRIDSTATOR_WORKER_UPDATE_FIXTURES to two actual rehearsal package directories",
        )?,
    );
    let temp = tempfile::Builder::new()
        .prefix("gridstator-worker-update-")
        .tempdir()?;
    let retained = std::env::var_os("GRIDSTATOR_KEEP_WORKER_UPDATE_EVIDENCE").is_some();
    let root = if retained {
        temp.keep()
    } else {
        temp.path().to_owned()
    };
    eprintln!("Independent worker update evidence: {}", root.display());
    let baseline = ReleaseManifest::read(
        packages
            .join("rehearsal-baseline")
            .join(release::RELEASE_MANIFEST_NAME),
    )?;
    let target = ReleaseManifest::read(
        packages
            .join("rehearsal-confirmed")
            .join(release::RELEASE_MANIFEST_NAME),
    )?;
    anyhow::ensure!(
        baseline.build_id != target.build_id,
        "need distinct real payloads"
    );
    let baseline_binary = packages.join("rehearsal-baseline-gridstator");
    let target_binary = packages.join("rehearsal-confirmed-gridstator");
    for (manifest, binary) in [(&baseline, &baseline_binary), (&target, &target_binary)] {
        manifest
            .target(release::host_target()?)?
            .executable
            .verify(binary)?;
        release::verify_executable_target(binary, release::host_target()?)?;
    }
    let directory = root.join("controller");
    let login = root.join("client.json");
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let output = std::process::Command::new(&target_binary)
        .args([
            "controller",
            "init",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--data-dir",
        ])
        .arg(&directory)
        .env("GRIDSTATOR_CONFIG", &login)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "actual controller init: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mirror = ArtifactMirror::new(directory.join("native-release-mirror"));
    mirror.cache_manifest(&target.to_bytes()?, &target.sha256()?)?;
    for (key, artifact) in &target.artifacts {
        mirror.cache_artifact(
            &target,
            key,
            packages
                .join("rehearsal-confirmed")
                .join(&artifact.archive.file),
        )?;
    }
    let database = gridstator::db::Db::open(&directory.join("controller.db"))?;
    database.write(|connection| gridstator::db::put(connection,"settings","cluster",&json!({"jobRetentionDays":90,"logRetentionDays":30,"snapshotRetentionDays":7,"autoUpdate":"off","updateWindow":null,"toolchain":{"act":"off"}}))).await?;
    drop(database);
    let controller_args: Vec<OsString> = vec![
        "controller".into(),
        "start".into(),
        "--data-dir".into(),
        directory.as_os_str().into(),
    ];
    let mut controller = OwnedUpdateRole::launch(
        &target_binary,
        &controller_args,
        &root.join("controller.log"),
        &login,
    )?;
    let mut config: Config = serde_json::from_slice(&std::fs::read(&login)?)?;
    config.url = format!("https://127.0.0.1:{port}");
    let client = Client::new(config)?;
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if client.get("/v1/health").await.is_ok() {
                return Ok::<_, anyhow::Error>(());
            }
            if let Some(status) = controller.0.try_wait()? {
                anyhow::bail!("controller exited {status}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("actual controller readiness")??;
    let server_build = client.get("/v1/updates").await?;
    anyhow::ensure!(
        server_build["controller"]["updatable"] == true,
        "verified mirror unavailable: {server_build}"
    );
    let mut node_ids = Vec::new();
    for (name, data) in [
        ("managed-standalone", root.join("managed-agent")),
        ("unmanaged-standalone", root.join("unmanaged-agent")),
    ] {
        let token = client
            .post("/v1/tokens", json!({"kind":"join","name":name,"maxUses":1}))
            .await?;
        let output = std::process::Command::new(&baseline_binary)
            .args([
                "agent",
                "join",
                &client.config.url,
                "--token",
                token["token"].as_str().unwrap(),
                "--fingerprint",
                &client.config.ca_fingerprint,
                "--name",
                name,
                "--data-dir",
            ])
            .arg(&data)
            .env_remove("GRIDSTATOR_SUPERVISED")
            .env_remove("GRIDSTATOR_INSTALL_ROOT")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "baseline join: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        node_ids.push(
            state::read_json(&data.join("agent.json"))?["nodeId"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let store = InstallStore::new(root.join("worker-install"));
    let archive = packages
        .join("rehearsal-baseline")
        .join(&baseline.target(release::host_target()?)?.archive.file);
    store.install_verified(&baseline, release::host_target()?, &archive)?;
    store.activate(&baseline.build_id, None)?;
    store.install_supervisor(&baseline_binary, false)?;
    let managed_args = vec![
        "supervise".into(),
        "--install-root".into(),
        store.root().as_os_str().into(),
        "--".into(),
        "agent".into(),
        "start".into(),
        "--data-dir".into(),
        root.join("managed-agent").into_os_string(),
    ];
    let managed = OwnedUpdateRole::launch(
        &store.supervisor_path(),
        &managed_args,
        &root.join("managed.log"),
        &login,
    )?;
    let unmanaged_args = vec![
        "agent".into(),
        "start".into(),
        "--data-dir".into(),
        root.join("unmanaged-agent").into_os_string(),
    ];
    let mut unmanaged = OwnedUpdateRole::launch(
        &baseline_binary,
        &unmanaged_args,
        &root.join("unmanaged.log"),
        &login,
    )?;
    let node_at = |nodes: &Value, index: usize| {
        nodes["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == node_ids[index])
            .cloned()
    };
    let nodes = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let nodes = client.get("/v1/nodes").await?;
            if [0, 1].iter().all(|index| {
                node_at(&nodes, *index).is_some_and(|node| {
                    node["status"] == "online"
                        && node["info"]["build"]["id"] == baseline.build_id
                        && node["update"]["state"] == "behind"
                })
            }) {
                return Ok::<_, anyhow::Error>(nodes);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("both baseline workers online")??;
    let managed_before = node_at(&nodes, 0).unwrap();
    let unmanaged_before = node_at(&nodes, 1).unwrap();
    let baseline_pid =
        managed_update_payload_pid(managed.0.id(), &store.executable(&baseline.build_id)?)?;
    anyhow::ensure!(
        managed_before["info"]["build"]["managed"] == true,
        "managed worker metadata: {managed_before}"
    );
    anyhow::ensure!(
        unmanaged_before["info"]["build"]["managed"] == false
            && unmanaged_before["update"]["state"] == "behind"
            && unmanaged_before["update"]["canUpdate"] == false,
        "unmanaged metadata: {unmanaged_before}"
    );
    anyhow::ensure!(
        unmanaged_before["update"]["detail"]
            .as_str()
            .unwrap_or("")
            .contains("managed"),
        "unmanaged install diagnosis: {unmanaged_before}"
    );
    let skipped = client
        .post("/v1/updates", json!({"nodes":[node_ids[1]]}))
        .await?;
    let skipped_node = skipped["rollout"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["nodeId"] == node_ids[1])
        .context("unmanaged rollout node")?;
    anyhow::ensure!(
        skipped_node["state"] == "skipped" && skipped_node["detail"] == "not a managed install",
        "unmanaged forced update: {skipped}"
    );
    // Keep the real launcher paused while its existing payload installs and requests restart.
    // This makes the source's prelaunch pending.attempts=0 assertion deterministic, then the
    // same production launcher resumes and starts/confirms the downloaded native payload.
    // Hold the install lock until the stop is observed: pausing a launcher while it owns that
    // lock would deadlock the very payload activation this gate is meant to inspect.
    let install_gate = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.root().join("install.lock"))?;
    fs2::FileExt::lock_exclusive(&install_gate)?;
    assert_eq!(
        unsafe { libc::kill(managed.0.id() as i32, libc::SIGSTOP) },
        0
    );
    let mut stopped_status = 0;
    assert_eq!(
        unsafe { libc::waitpid(managed.0.id() as i32, &mut stopped_status, libc::WUNTRACED) },
        managed.0.id() as i32
    );
    anyhow::ensure!(
        libc::WIFSTOPPED(stopped_status),
        "owned launcher did not stop at gate"
    );
    fs2::FileExt::unlock(&install_gate)?;
    client
        .request(
            reqwest::Method::PATCH,
            "/v1/settings",
            Some(json!({"autoUpdate":"follow"})),
        )
        .await?;
    let pending_observed = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(state) = store.state()?
                && state.current == target.build_id
                && state.pending.is_some()
            {
                return Ok::<_, anyhow::Error>(state);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("downloaded worker activated before launcher resumes")??;
    let provisional = pending_observed.pending.as_ref().unwrap();
    anyhow::ensure!(
        pending_observed.previous.as_deref() == Some(&baseline.build_id)
            && provisional.build_id == target.build_id
            && provisional.previous == baseline.build_id
            && provisional.attempts == 0
            && provisional.first_launch_at.is_none()
            && provisional.deadline.is_none(),
        "prelaunch pending activation: {pending_observed:?}"
    );
    let updating = client.get("/v1/nodes").await?;
    anyhow::ensure!(
        node_at(&updating, 0).context("updating managed node")?["update"]["state"] == "updating",
        "managed node not updating before restart: {updating}"
    );
    assert_eq!(
        unsafe { libc::kill(managed.0.id() as i32, libc::SIGCONT) },
        0
    );
    let final_view = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(state) = store.state()?
                && state.current == target.build_id
                && state.pending.is_none()
            {
                let nodes = client.get("/v1/nodes").await?;
                let node = node_at(&nodes, 0).context("updated worker node")?;
                let updates = client.get("/v1/updates").await?;
                if node["status"] == "online"
                    && node["info"]["build"]["id"] == target.build_id
                    && node["info"]["build"]["managed"] == true
                    && node["update"]["state"] == "current"
                    && updates["rollout"]["state"] == "finished"
                {
                    return Ok::<_, anyhow::Error>((node, updates));
                }
            }
            anyhow::ensure!(
                unmanaged.0.try_wait()?.is_none(),
                "unmanaged worker unexpectedly exited/restarted"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("standalone worker install/restart/rejoin/confirmation")??;
    let supervisor_digest = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok((size, digest)) = release::hash_file(store.supervisor_path())
                && digest == target.target(release::host_target()?)?.executable.sha256
            {
                return Ok::<_, anyhow::Error>((size, digest));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("healthy worker refreshes stable supervisor to the verified target")??;
    let target_pid =
        managed_update_payload_pid(managed.0.id(), &store.executable(&target.build_id)?)?;
    anyhow::ensure!(
        baseline_pid != target_pid,
        "managed payload did not restart"
    );
    let installed = store.state()?.context("final install state")?;
    anyhow::ensure!(
        installed.previous.as_deref() == Some(&baseline.build_id)
            && installed.current == target.build_id
            && installed.pending.is_none()
            && installed.last_rollback.is_none(),
        "managed transition: {installed:?}"
    );
    let pending = pending_observed;
    anyhow::ensure!(
        pending.pending.as_ref().unwrap().previous == baseline.build_id
            && pending.pending.as_ref().unwrap().build_id == target.build_id,
        "pending transition: {pending:?}"
    );
    anyhow::ensure!(
        unmanaged.0.try_wait()?.is_none(),
        "unmanaged worker restarted"
    );
    let remaining = node_at(&client.get("/v1/nodes").await?, 1).unwrap();
    anyhow::ensure!(
        remaining["info"]["build"]["id"] == baseline.build_id
            && remaining["info"]["build"]["managed"] == false,
        "unmanaged build changed: {remaining}"
    );
    std::fs::write(
        root.join("worker-update-evidence.json"),
        serde_json::to_vec_pretty(
            &json!({"scope":"actual standalone managed worker, separate install/supervisor from controller; host runtime only; non-host archive entries are parsing fixtures","target":release::host_target()?,"baseline":baseline,"targetManifest":target,"sourcePackages":packages,"pendingObserved":pending,"finalInstall":installed,"payloadPids":{"baseline":baseline_pid,"target":target_pid},"supervisorAfter":{"size":supervisor_digest.0,"sha256":supervisor_digest.1},"updatedNode":final_view.0,"updates":final_view.1,"forcedUnmanaged":skipped,"unmanagedNode":remaining}),
        )?,
    )?;
    eprintln!(
        "Independent worker update passed: {}",
        root.join("worker-update-evidence.json").display()
    );
    Ok(())
}
