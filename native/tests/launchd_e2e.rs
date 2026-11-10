//! Explicit actual-macOS proofs. Every role, store, label and plist is private to this test.
//! The single-host installation fixture does not establish other targets or release signing.
#![cfg(target_os = "macos")]
use anyhow::{Context, Result, ensure};
use gridstator::{
    client::{Client, Config},
    controller,
    install::InstallStore,
    release::{self, FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest},
    services::{ServiceAction, ServiceConfig, ServiceManager, ServicePlatform, ServiceRole},
    state, worker,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

fn root() -> Result<PathBuf> {
    let parent = if let Some(parent) = std::env::var_os("GRIDSTATOR_MAC_VALIDATION_ROOT") {
        let parent = PathBuf::from(parent);
        state::private_dir(&parent)?;
        parent
    } else {
        // The platform temporary directory belongs to the OS. Only the new fixture is private.
        std::env::temp_dir()
    };
    Ok(tempfile::Builder::new()
        .prefix("native-mac-role-")
        .tempdir_in(parent)?
        .keep())
}
fn binary() -> PathBuf {
    std::env::var_os("GRIDSTATOR_TEST_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_gridstator").into())
}
fn command(
    binary: &Path,
    args: &[&str],
    data: &Path,
    login: &Path,
    log: &Path,
) -> Result<std::process::Child> {
    let output = fs::File::create(log)?;
    Ok(Command::new(binary)
        .args(args)
        .arg("--data-dir")
        .arg(data)
        .env("GRIDSTATOR_CONFIG", login)
        .env(
            "GRIDSTATOR_DOCKER",
            "/nonexistent/gridstator-mac-proof-docker",
        )
        .env("GRIDSTATOR_ACT", "/nonexistent/gridstator-mac-proof-act")
        .env_remove("GRIDSTATOR_SUPERVISED")
        .env_remove("GRIDSTATOR_INSTALL_ROOT")
        .env_remove("GRIDSTATOR_INSTALL_VERSION")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .spawn()?)
}
struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            // SAFETY: only the owned live child PID is signalled.
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
async fn ready(data: &Path, initial: &Value, previous: Option<u64>) -> Result<(Client, u64)> {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if let Ok(runtime) = state::read_json(&data.join("runtime.json")) {
                let pid = runtime["pid"].as_u64().context("controller PID")?;
                if Some(pid) != previous {
                    let client = Client::new(Config {
                        version: 1,
                        url: format!(
                            "https://127.0.0.1:{}",
                            runtime["port"].as_u64().context("port")?
                        ),
                        ca_pem: fs::read_to_string(data.join("pki/ca.pem"))?,
                        ca_fingerprint: initial["caFingerprint"].as_str().unwrap().into(),
                        token: initial["token"].as_str().unwrap().into(),
                        token_name: None,
                    })?;
                    if client.get("/v1/health").await.is_ok() {
                        return Ok::<_, anyhow::Error>((client, pid));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("native controller readiness")?
}
async fn pair(client: &Client, data: &Path) -> Result<String> {
    let token = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"private-mac-proof","maxUses":1}),
        )
        .await?;
    let joined = worker::join(
        data,
        &client.config.url,
        token["token"].as_str().unwrap(),
        &client.config.ca_fingerprint,
        Some(format!("mac-proof-{}", state::id())),
        None,
    )
    .await?;
    Ok(joined["nodeId"].as_str().unwrap().into())
}
async fn online(client: &Client, id: &str, after: Option<i64>) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let nodes = client.get("/v1/nodes").await?;
            if let Some(node) = nodes["nodes"].as_array().unwrap().iter().find(|node| {
                node["id"] == id
                    && node["status"] == "online"
                    && after.is_none_or(|before| {
                        node["connectedAt"].as_i64().is_some_and(|now| now > before)
                    })
            }) {
                return Ok::<_, anyhow::Error>(node.clone());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("native worker hello")?
}
fn assert_unavailable_node(node: &Value) {
    assert_eq!(node["info"]["platform"], "darwin");
    assert_eq!(
        node["info"]["arch"],
        if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        }
    );
    assert_eq!(node["info"]["build"]["runtime"], "rust");
    assert_eq!(node["info"]["docker"]["available"], false);
    assert_eq!(node["info"]["executors"], json!([]));
    assert!(node["info"]["executionCapacity"].is_null());
    assert!(node["info"]["cpus"].as_u64().unwrap() > 0);
    assert!(node["info"]["memoryTotalMb"].as_u64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "starts isolated actual macOS CLI roles; run explicitly on an authorized Mac"]
async fn actual_mac_foreground_controller_worker_without_docker_and_clean_shutdown() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = root()?;
    let data = root.join("controller");
    let login = root.join("client.json");
    let initial = controller::initialize(&data, "127.0.0.1", 0, &[], false, &login).await?;
    let mut server = OwnedChild(command(
        &binary(),
        &["controller", "start"],
        &data,
        &login,
        &root.join("controller.log"),
    )?);
    let (client, pid) = ready(&data, &initial, None).await?;
    let agent_data = root.join("agent");
    let id = pair(&client, &agent_data).await?;
    let mut agent = OwnedChild(command(
        &binary(),
        &["agent", "start"],
        &agent_data,
        &login,
        &root.join("agent.log"),
    )?);
    let node = online(&client, &id, None).await?;
    assert_unavailable_node(&node);
    assert_eq!(node["info"]["build"]["managed"], false);
    // SAFETY: positive live PID belongs to the owned agent CLI child.
    ensure!(
        unsafe { libc::kill(agent.0.id() as i32, libc::SIGTERM) } == 0,
        "agent signal"
    );
    let exit = tokio::task::spawn_blocking(move || agent.0.wait()).await??;
    ensure!(exit.success(), "foreground worker shutdown: {exit}");
    ensure!(
        unsafe { libc::kill(server.0.id() as i32, libc::SIGTERM) } == 0,
        "controller signal"
    );
    let exit = tokio::task::spawn_blocking(move || server.0.wait()).await??;
    ensure!(exit.success(), "foreground controller shutdown: {exit}");
    ensure!(
        !data.join("runtime.json").exists(),
        "stopped controller retains runtime identity"
    );
    fs::write(
        root.join("foreground-evidence.json"),
        serde_json::to_vec_pretty(
            &json!({"scope":"actual foreground native macOS CLI/controller/worker; intentionally missing Docker; no signing claim","target":release::host_target()?,"node":node,"controllerPid":pid}),
        )?,
    )?;
    eprintln!("Mac foreground proof: {}", root.display());
    Ok(())
}

struct OwnedService {
    manager: ServiceManager,
    config: ServiceConfig,
    payloads: Vec<u64>,
}
impl Drop for OwnedService {
    fn drop(&mut self) {
        let _ = self.manager.uninstall(&self.config);
        // Failure cleanup is restricted to previously observed payloads in this test's managed store.
        let system = sysinfo::System::new_all();
        for pid in &self.payloads {
            if let Some(process) = system.process(sysinfo::Pid::from_u32(*pid as u32))
                && process
                    .exe()
                    .is_some_and(|path| path.starts_with(&self.config.install_root))
            {
                unsafe {
                    libc::kill(*pid as i32, libc::SIGTERM);
                }
            }
        }
    }
}
fn service(root: &Path, role: ServiceRole, data: &Path, login: &Path) -> Result<OwnedService> {
    let binary = binary();
    let bytes = Command::new(&binary)
        .args(["build-info", "--json"])
        .output()?;
    ensure!(bytes.status.success(), "native build-info");
    let info: Value = serde_json::from_slice(&bytes.stdout)?;
    let artifact = ReleaseArtifact {
        archive: FileDigest::from_path("single-host-fixture", &binary)?,
        executable: FileDigest::from_path("gridstator", &binary)?,
    };
    let manifest = ReleaseManifest {
        schema_version: 1,
        build_id: info["buildId"].as_str().unwrap().into(),
        version: info["version"].as_str().unwrap().into(),
        source_revision: info["sourceRevision"].as_str().unwrap().into(),
        protocol: ProtocolCompatibility::default(),
        artifacts: release::SUPPORTED_TARGETS
            .into_iter()
            .map(|target| (target.into(), artifact.clone()))
            .collect(),
    };
    let store = InstallStore::new(root.join(format!("{}-install", role.as_str())));
    store.install_verified(&manifest, release::host_target()?, &binary)?;
    store.activate(&manifest.build_id, None)?;
    let supervisor = store.install_supervisor(&binary, false)?;
    let mut config = ServiceConfig::new(
        role,
        supervisor,
        store.root().into(),
        data.into(),
        dirs::home_dir().context("home")?,
        unsafe { libc::geteuid() },
    );
    config.label = format!(
        "org.gridstator.native.validation.{}.{}",
        role.as_str(),
        state::id().to_lowercase()
    );
    config.name = format!("gridstator-mac-validation-{}", state::id().to_lowercase());
    config.environment.insert(
        "GRIDSTATOR_CONFIG".into(),
        login.to_string_lossy().into_owned(),
    );
    config.environment.insert(
        "GRIDSTATOR_DOCKER".into(),
        "/nonexistent/gridstator-mac-proof-docker".into(),
    );
    config.environment.insert(
        "GRIDSTATOR_ACT".into(),
        "/nonexistent/gridstator-mac-proof-act".into(),
    );
    let manager = ServiceManager::new(ServicePlatform::Launchd);
    let path = config.definition_path(ServicePlatform::Launchd)?;
    ensure!(!path.exists(), "unique validation plist collision");
    fs::write(
        root.join(format!("{}.plist", role.as_str())),
        manager.render(&config)?,
    )?;
    let owned = OwnedService {
        manager,
        config,
        payloads: vec![],
    };
    owned.manager.install(&owned.config, false)?;
    Ok(owned)
}
fn live(pid: u64) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
async fn stopped(pid: u64) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        while live(pid) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("owned payload stopped")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "installs unique temporary user LaunchAgents; requires authorized actual macOS GUI domain"]
async fn actual_launchd_controller_and_worker_start_restart_stop_and_uninstall() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = root()?;
    let data = root.join("controller");
    let login = root.join("client.json");
    // The standalone worker must retain a stable endpoint across the controller restart.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let initial = controller::initialize(&data, "127.0.0.1", port, &[], false, &login).await?;
    let mut server = service(&root, ServiceRole::Controller, &data, &login)?;
    let (client, pid) = ready(&data, &initial, None).await?;
    server.payloads.push(pid);
    let agent_data = root.join("agent");
    let id = pair(&client, &agent_data).await?;
    let agent = service(&root, ServiceRole::Agent, &agent_data, &login)?;
    let node = online(&client, &id, None).await?;
    assert_unavailable_node(&node);
    assert_eq!(node["info"]["build"]["managed"], true);
    let old_connection = node["connectedAt"].as_i64().context("connection time")?;
    agent
        .manager
        .control(&agent.config, ServiceAction::Restart)?;
    let restarted_node = online(&client, &id, Some(old_connection)).await?;
    assert_unavailable_node(&restarted_node);
    server
        .manager
        .control(&server.config, ServiceAction::Restart)?;
    let (client, restarted) = ready(&data, &initial, Some(pid)).await?;
    server.payloads.push(restarted);
    stopped(pid).await?;
    online(&client, &id, None).await?;
    agent.manager.control(&agent.config, ServiceAction::Stop)?;
    server
        .manager
        .control(&server.config, ServiceAction::Stop)?;
    stopped(restarted).await?;
    ensure!(
        !data.join("runtime.json").exists(),
        "launchd stop left runtime identity"
    );
    let labels = [server.config.label.clone(), agent.config.label.clone()];
    let definitions = [
        server.config.definition_path(ServicePlatform::Launchd)?,
        agent.config.definition_path(ServicePlatform::Launchd)?,
    ];
    drop(agent);
    drop(server);
    for path in definitions {
        ensure!(
            !path.exists(),
            "validation plist retained: {}",
            path.display()
        );
    }
    for label in &labels {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if !Command::new("launchctl")
                    .args([
                        "print",
                        &format!("gui/{}/{label}", unsafe { libc::geteuid() }),
                    ])
                    .output()?
                    .status
                    .success()
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .with_context(|| format!("validation label retained: {label}"))??;
    }
    fs::write(
        root.join("launchd-evidence.json"),
        serde_json::to_vec_pretty(
            &json!({"scope":"actual macOS user launchd native controller and standalone worker, unique labels; no reboot/system-daemon/signing claim","target":release::host_target()?,"labels":labels,"node":node,"restartedNode":restarted_node,"controllerPids":[pid,restarted],"uninstalled":true}),
        )?,
    )?;
    eprintln!("Mac launchd proof: {}", root.display());
    Ok(())
}
