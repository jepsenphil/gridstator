//! Opt-in actual user-service proof. Every installed unit has a unique test-owned name.
#![cfg(target_os = "linux")]
use anyhow::{Context, Result, ensure};
use gridstator::{
    client::{Client, Config},
    controller,
    install::InstallStore,
    release::{self, FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest},
    services::{
        ServiceAction, ServiceConfig, ServiceManager, ServicePlatform, ServiceRole, ServiceScope,
    },
    state,
};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

struct OwnedService {
    manager: ServiceManager,
    config: ServiceConfig,
}
impl Drop for OwnedService {
    fn drop(&mut self) {
        let _ = self.manager.uninstall(&self.config);
        let mut command = std::process::Command::new("systemctl");
        if self.config.scope == ServiceScope::User {
            command.arg("--user");
        }
        let _ = command
            .args(["reset-failed", &self.config.unit_name()])
            .output();
    }
}
async fn ready(directory: &Path, original: &Value, previous: Option<u64>) -> Result<(Client, u64)> {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if let Ok(runtime) = state::read_json(&directory.join("runtime.json")) {
                let pid = runtime["pid"].as_u64().context("runtime PID")?;
                if Some(pid) != previous {
                    let client = Client::new(Config {
                        version: 1,
                        url: format!("https://127.0.0.1:{}", runtime["port"].as_u64().unwrap()),
                        ca_pem: fs::read_to_string(directory.join("pki/ca.pem"))?,
                        ca_fingerprint: original["caFingerprint"].as_str().unwrap().into(),
                        token: original["token"].as_str().unwrap().into(),
                        token_name: None,
                    })?;
                    if client.get("/v1/health").await.is_ok() {
                        break Ok((client, pid));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await?
}
#[tokio::test]
#[ignore = "installs a unique temporary user or root systemd unit; requires systemd and Docker"]
async fn actual_systemd_supervisor_controller_and_embedded_worker_lifecycle() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let system_scope = std::env::var("GRIDSTATOR_SYSTEMD_SCOPE").as_deref() == Ok("system");
    if system_scope {
        ensure!(
            unsafe { libc::geteuid() } == 0,
            "system scope requires root"
        );
    } else {
        ensure!(
            std::process::Command::new("systemctl")
                .args(["--user", "show-environment"])
                .output()?
                .status
                .success(),
            "requires a running user systemd manager"
        );
    }
    ensure!(
        std::process::Command::new("docker")
            .arg("info")
            .output()?
            .status
            .success(),
        "requires a ready Docker daemon"
    );
    let temporary = tempfile::tempdir()?;
    let directory = temporary.path().join("controller");
    let login = temporary.path().join("client.json");
    // The embedded worker keeps the controller URL across service restarts. A port-zero
    // listener would choose a different port on restart and test an impossible saved URL.
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let initialized =
        controller::initialize(&directory, "127.0.0.1", port, &[], false, &login).await?;
    let binary_path = temporary.path().join("gridstator");
    fs::copy(
        std::env::var_os("GRIDSTATOR_TEST_BINARY")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_gridstator").into()),
        &binary_path,
    )?;
    let binary = binary_path.as_path();
    let info: Value = serde_json::from_slice(
        &std::process::Command::new(binary)
            .args(["build-info", "--json"])
            .output()?
            .stdout,
    )?;
    // Single-host managed installation fixture. Other manifest entries are never executed and
    // are not evidence for those platforms; the release producer is tested separately.
    let artifact = ReleaseArtifact {
        archive: FileDigest::from_path("fixture-native", binary)?,
        executable: FileDigest::from_path("gridstator", binary)?,
    };
    let manifest = ReleaseManifest {
        schema_version: 1,
        build_id: info["buildId"].as_str().context("binary build ID")?.into(),
        version: info["version"].as_str().context("binary version")?.into(),
        source_revision: info["sourceRevision"]
            .as_str()
            .context("binary source revision")?
            .into(),
        protocol: ProtocolCompatibility::default(),
        artifacts: release::SUPPORTED_TARGETS
            .into_iter()
            .map(|target| (target.into(), artifact.clone()))
            .collect(),
    };
    let store = InstallStore::new(temporary.path().join("managed store"));
    store.install_verified(&manifest, release::host_target()?, binary)?;
    store.activate(&manifest.build_id, None)?;
    let supervisor = store.install_supervisor(binary, false)?;
    let mut config = ServiceConfig::new(
        ServiceRole::Controller,
        supervisor,
        store.root().to_owned(),
        directory.clone(),
        dirs::home_dir().context("home directory required")?,
        unsafe { libc::geteuid() },
    );
    if system_scope {
        config.scope = ServiceScope::System;
        config.user = Some("root".into());
    }
    config.name = format!("gridstator-native-proof-{}", state::id().to_lowercase());
    config.environment.insert(
        "GRIDSTATOR_CONFIG".into(),
        login.to_string_lossy().into_owned(),
    );
    config.environment.insert(
        "GRIDSTATOR_AGENT_DATA".into(),
        temporary
            .path()
            .join("agent")
            .to_string_lossy()
            .into_owned(),
    );
    config.extra_args = vec![
        "--with-agent".into(),
        "--agent-name".into(),
        "systemd-proof-worker".into(),
    ];
    let service = OwnedService {
        manager: ServiceManager::new(ServicePlatform::Systemd),
        config,
    };
    service.manager.install(&service.config, false)?;
    let (client, pid) = ready(&directory, &initialized, None).await?;
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let nodes = client.get("/v1/nodes").await?;
            if nodes["nodes"].as_array().is_some_and(|nodes| {
                nodes.iter().any(|node| {
                    node["status"] == "online" && node["info"]["docker"]["available"] == true
                })
            }) {
                break Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let job=client.post("/v1/jobs",json!({"spec":{"kind":"command","image":"alpine:3.21","command":["echo","native-systemd"],"resources":{"cpus":1,"memoryMb":64}}})).await?;
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let current = client
                .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
                .await?;
            if gridstator::protocol::terminal(current["state"].as_str().unwrap_or("")) {
                ensure!(
                    current["state"] == "succeeded",
                    "service worker job failed: {current}"
                );
                break Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    ensure!(
        service
            .manager
            .install(&service.config, false)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists),
        "existing service was replaced without explicit force"
    );
    service.manager.install(&service.config, true)?;
    let (client, replaced) = ready(&directory, &initialized, Some(pid)).await?;
    ensure!(
        client
            .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
            .await?["state"]
            == "succeeded",
        "replacing service lost completed job history"
    );
    service
        .manager
        .control(&service.config, ServiceAction::Restart)?;
    let (_, restarted) = ready(&directory, &initialized, Some(replaced)).await?;
    ensure!(replaced != restarted, "service payload did not restart");
    service
        .manager
        .control(&service.config, ServiceAction::Stop)?;
    let mut command = std::process::Command::new("systemctl");
    if !system_scope {
        command.arg("--user");
    }
    let status = command
        .args([
            "show",
            &service.config.unit_name(),
            "--property=ActiveState",
            "--value",
        ])
        .output()?;
    ensure!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim() == "inactive",
        "graceful service stop must be inactive, not failed: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    ensure!(
        !directory.join("runtime.json").exists(),
        "controller did not clean runtime identity on stop"
    );
    drop(service);
    Ok(())
}
