//! Real native role startup and launcher refresh, using only disposable loopback state.
use anyhow::{Context, Result, ensure};
use gridstator::{
    client::{Client, Config},
    install::InstallStore,
    release::{
        FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest, SUPPORTED_TARGETS,
        host_target,
    },
    state,
};
use serde_json::json;
use std::{fs, path::Path, process::Stdio, time::Duration};
use tokio::process::{Child, Command};

struct OwnedProcess(Child);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.0.id() {
            // SAFETY: the owned live supervisor receives its normal termination signal.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        for _ in 0..200 {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.0.start_kill();
    }
}
fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .current_dir(root)
        .env("GRIDSTATOR_CONFIG", root.join("client.json"))
        .env("GRIDSTATOR_CONTROLLER_DATA", root.join("controller"))
        .env("GRIDSTATOR_AGENT_DATA", root.join("worker"))
        .env_remove("GRIDSTATOR_URL")
        .env_remove("GRIDSTATOR_TOKEN")
        .env_remove("GRIDSTATOR_CA_FILE")
        .env_remove("GRIDSTATOR_SUPERVISED")
        .env_remove("GRIDSTATOR_INSTALL_ROOT")
        .env_remove("GRIDSTATOR_INSTALL_VERSION")
        .env(
            "GRIDSTATOR_DOCKER",
            "/nonexistent/private-role-contract-docker",
        )
        .env("GRIDSTATOR_ACT", "/nonexistent/private-role-contract-act");
    command
}
async fn run(root: &Path, arguments: &[&str]) -> Result<()> {
    let result = command(root).args(arguments).output().await?;
    ensure!(
        result.status.success(),
        "native command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
fn installation(root: &Path) -> Result<InstallStore> {
    let executable = Path::new(env!("CARGO_BIN_EXE_gridstator"));
    let digest = FileDigest::from_path("gridstator", executable)?;
    // Direct executable adoption verifies the actual host file. Unconsumed archive
    // and non-host members are parser fixtures, without archive/other-target claims.
    let artifact = ReleaseArtifact {
        archive: FileDigest {
            file: "unconsumed-fixture.tar.gz".into(),
            ..digest.clone()
        },
        executable: digest,
    };
    let manifest = ReleaseManifest {
        schema_version: 1,
        build_id: gridstator::BUILD_ID.into(),
        version: gridstator::VERSION.into(),
        source_revision: gridstator::SOURCE_REVISION.into(),
        protocol: ProtocolCompatibility::default(),
        artifacts: SUPPORTED_TARGETS
            .into_iter()
            .map(|target| (target.into(), artifact.clone()))
            .collect(),
    };
    let store = InstallStore::open(root)?;
    store.install_verified_executable(&manifest, host_target()?, executable)?;
    store.activate(gridstator::BUILD_ID, None)?;
    fs::write(
        store.supervisor_path(),
        b"old stable launcher bytes; do not execute",
    )?;
    Ok(store)
}
fn start(root: &Path, store: &InstallStore, role: &str, data: &Path) -> Result<OwnedProcess> {
    let log = fs::File::create(root.join(format!("{role}.log")))?;
    // The real supervisor runs from the current CLI while the old stable file
    // remains in place. Only a healthy installed role may replace that file.
    let child = command(root)
        .arg("supervise")
        .arg("--install-root")
        .arg(store.root())
        .args(["--", role, "start", "--data-dir"])
        .arg(data)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    Ok(OwnedProcess(child))
}
async fn refreshed(store: &InstallStore) -> Result<()> {
    let expected = store
        .manifest(gridstator::BUILD_ID)?
        .target(host_target()?)?
        .executable
        .clone();
    let path = store.supervisor_path();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let expected = expected.clone();
            let path = path.clone();
            if tokio::task::spawn_blocking(move || expected.verify(path))
                .await?
                .is_ok()
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("healthy role never refreshed the stable native supervisor")?
}

#[tokio::test]
async fn actual_managed_controller_and_worker_refresh_the_verified_stable_supervisor() -> Result<()>
{
    let temporary = tempfile::tempdir()?;
    let root = temporary.path();
    let controller_data = root.join("controller");
    run(
        root,
        &[
            "controller",
            "init",
            "--data-dir",
            controller_data.to_str().unwrap(),
            "--host",
            "127.0.0.1",
            "--port",
            "0",
        ],
    )
    .await?;
    let controller_store = installation(&root.join("controller-install"))?;
    let _controller = start(root, &controller_store, "controller", &controller_data)?;
    let client = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(runtime) = state::read_json(&controller_data.join("runtime.json")) {
                let mut config: Config =
                    serde_json::from_slice(&fs::read(root.join("client.json"))?)?;
                config.url = format!(
                    "https://127.0.0.1:{}",
                    runtime["port"].as_u64().context("port")?
                );
                let client = Client::new(config)?;
                if client.get("/v1/health").await.is_ok() {
                    return Ok::<_, anyhow::Error>(client);
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await??;
    refreshed(&controller_store).await?;
    assert!(controller_store.state()?.unwrap().pending.is_none());
    client
        .request(
            reqwest::Method::PATCH,
            "/v1/settings",
            Some(json!({"autoUpdate":"off","toolchain":{"act":"off"}})),
        )
        .await?;
    let cluster = client.get("/v1/cluster").await?;
    assert_eq!(cluster["build"]["id"], gridstator::BUILD_ID);
    let token = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"managed-role-proof","maxUses":1}),
        )
        .await?;
    let worker_data = root.join("worker");
    run(
        root,
        &[
            "agent",
            "join",
            &client.config.url,
            "--data-dir",
            worker_data.to_str().unwrap(),
            "--token",
            token["token"].as_str().unwrap(),
            "--fingerprint",
            &client.config.ca_fingerprint,
            "--name",
            "actual-private-managed-worker",
        ],
    )
    .await?;
    let worker_store = installation(&root.join("worker-install"))?;
    let _worker = start(root, &worker_store, "agent", &worker_data)?;
    refreshed(&worker_store).await?;
    let node = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let nodes = client.get("/v1/nodes").await?;
            if let Some(node) = nodes["nodes"].as_array().and_then(|nodes| {
                nodes.iter().find(|node| {
                    node["name"] == "actual-private-managed-worker" && node["status"] == "online"
                })
            }) {
                return Ok::<_, anyhow::Error>(node.clone());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await??;
    assert_eq!(node["info"]["build"]["id"], gridstator::BUILD_ID);
    assert_eq!(node["info"]["build"]["managed"], true);
    assert_eq!(node["info"]["build"]["runtime"], "rust");
    assert_eq!(node["info"]["docker"]["available"], false);
    assert!(node["info"]["executors"].as_array().unwrap().is_empty());
    let job = client
        .post(
            "/v1/jobs",
            json!({"spec":{"kind":"command","command":["true"],"target":{"node":node["id"]}}}),
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        client
            .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
            .await?["state"],
        "queued"
    );
    assert_eq!(
        client
            .post(
                &format!("/v1/jobs/{}/cancel", job["id"].as_str().unwrap()),
                json!({})
            )
            .await?["state"],
        "cancelled"
    );
    assert!(worker_store.state()?.unwrap().pending.is_none());
    println!(
        "actual native controller and worker refreshed both verified stable launchers after health/welcome; worker correctly waits for Docker"
    );
    Ok(())
}
