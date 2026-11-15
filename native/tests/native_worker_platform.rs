//! Native worker portability. Header fixtures test parsing, never target-host execution.
use anyhow::{Context, Result, ensure};
use gridstator::{
    client::{Client, Config},
    controller, local_worker, release, state,
    updates::act,
    worker,
};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path, process::Stdio, time::Duration};

fn pe(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes[68..70].copy_from_slice(&machine.to_le_bytes());
    bytes[84..86].copy_from_slice(&2_u16.to_le_bytes());
    bytes[86..88].copy_from_slice(&2_u16.to_le_bytes());
    bytes[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
    bytes
}

#[test]
fn windows_identity_checks_pe_architecture_and_rejects_dlls_and_truncation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let binary = root.path().join("gridstator.exe");
    assert_eq!(
        release::target_for("windows", "x86_64"),
        Some("windows/amd64")
    );
    assert_eq!(
        release::target_for("win32", "aarch64"),
        Some("windows/arm64")
    );
    for (machine, target) in [(0x8664, "windows/amd64"), (0xaa64, "windows/arm64")] {
        let mut bytes = pe(machine);
        fs::write(&binary, &bytes)?;
        release::verify_executable_target(&binary, target)?;
        assert!(release::verify_executable_target(&binary, "linux/amd64").is_err());
        let other = if machine == 0x8664 {
            "windows/arm64"
        } else {
            "windows/amd64"
        };
        assert!(release::verify_executable_target(&binary, other).is_err());
        bytes[87] |= 0x20; // IMAGE_FILE_DLL
        fs::write(&binary, &bytes)?;
        assert!(release::verify_executable_target(&binary, target).is_err());
        fs::write(&binary, &bytes[..70])?;
        assert!(release::verify_executable_target(&binary, target).is_err());
    }
    Ok(())
}

fn act_zip(path: &Path, members: &[(&str, Vec<u8>)]) -> Result<()> {
    let mut zip = zip::ZipWriter::new(fs::File::create(path)?);
    for (name, bytes) in members {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(())
}

#[test]
fn managed_windows_act_extracts_only_verified_executable_and_rejects_unsafe_members() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let archive = root.path().join("act.zip");
    let destination = root.path().join("tools/act.exe");
    let bytes = pe(0x8664);
    act_zip(
        &archive,
        &[("LICENSE", b"license".to_vec()), ("act.exe", bytes.clone())],
    )?;
    assert_eq!(
        act::install_archive(&archive, &destination, "windows/amd64")?,
        release::sha256_bytes(&bytes)
    );
    assert_eq!(fs::read(&destination)?, bytes);
    assert!(!root.path().join("tools/LICENSE").exists());
    assert!(act::install_archive(&archive, &destination, "windows/arm64").is_err());
    act_zip(&archive, &[("../outside", vec![1]), ("act.exe", bytes)])?;
    assert!(act::install_archive(&archive, &destination, "windows/amd64").is_err());
    assert!(!root.path().join("outside").exists());
    Ok(())
}

struct Child(tokio::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

fn command(root: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .env("GRIDSTATOR_CONFIG", root.join("client.json"))
        .env("GRIDSTATOR_AGENT_DATA", root.join("agent"))
        .env("GRIDSTATOR_CONTROLLER_DATA", root.join("controller"))
        .env("GRIDSTATOR_DOCKER", root.join("missing-docker"))
        .env("GRIDSTATOR_ACT", root.join("missing-act"))
        .env_remove("GRIDSTATOR_INSTALL_ROOT")
        .env_remove("GRIDSTATOR_INSTALL_VERSION")
        .env_remove("GRIDSTATOR_SUPERVISED")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command
}

#[tokio::test]
async fn native_worker_joins_advertises_and_accepts_local_controls_without_wsl() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = tempfile::tempdir()?;
    let dir = root.path().join("controller");
    let initialized = controller::initialize(
        &dir,
        "127.0.0.1",
        0,
        &[],
        false,
        &root.path().join("client.json"),
    )
    .await?;
    let mut controller = Child(
        command(root.path())
            .args(["controller", "start", "--data-dir"])
            .arg(&dir)
            .spawn()?,
    );
    let runtime = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(runtime) = state::read_json(&dir.join("runtime.json")) {
                return Ok::<_, anyhow::Error>(runtime);
            }
            ensure!(
                controller.0.try_wait()?.is_none(),
                "controller exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await??;
    let client = Client::new(Config {
        version: 1,
        url: format!(
            "https://127.0.0.1:{}",
            runtime["port"].as_u64().context("controller port")?
        ),
        ca_pem: fs::read_to_string(dir.join("pki/ca.pem"))?,
        ca_fingerprint: initialized["caFingerprint"].as_str().unwrap().into(),
        token: initialized["token"].as_str().unwrap().into(),
        token_name: None,
    })?;
    let token = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"native-platform-worker","uses":1}),
        )
        .await?;
    let agent_dir = root.path().join("agent");
    let joined = worker::join(
        &agent_dir,
        &client.config.url,
        token["token"].as_str().unwrap(),
        &client.config.ca_fingerprint,
        Some("native-platform-worker".into()),
        None,
    )
    .await?;
    let mut worker = Child(
        command(root.path())
            .args(["agent", "start", "--data-dir"])
            .arg(&agent_dir)
            .spawn()?,
    );
    let status = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response =
                local_worker::execute(json!({"action":"status","dataDir":agent_dir})).await?;
            let snapshot = response["workers"][0].clone();
            if snapshot["running"] == true && snapshot["connected"] == true {
                return Ok::<Value, anyhow::Error>(snapshot);
            }
            ensure!(
                worker.0.try_wait()?.is_none(),
                "worker exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    assert_eq!(status["nodeId"], joined["nodeId"]);
    assert_eq!(status["info"]["build"]["target"], release::host_target()?);
    assert_eq!(status["state"], "waitingForDocker");
    #[cfg(windows)]
    assert_eq!(status["info"]["platform"], "windows");
    let paused = local_worker::execute(json!({"action":"pause","dataDir":agent_dir})).await?;
    assert_eq!(paused["workers"][0]["paused"], true);
    let resumed = local_worker::execute(json!({"action":"resume","dataDir":agent_dir})).await?;
    assert_eq!(resumed["workers"][0]["paused"], false);
    local_worker::execute(json!({"action":"stop","dataDir":agent_dir})).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(15), worker.0.wait())
            .await??
            .success()
    );
    controller.0.kill().await?;
    Ok(())
}
