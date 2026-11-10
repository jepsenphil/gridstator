//! Editor onboarding through real private bridge processes and a simulated user service manager.
//! No real OS service is installed, stopped, or started by these tests.
#![cfg(target_os = "linux")]

use anyhow::{Result, ensure};
use gridstator::{
    release::{
        FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest, SUPPORTED_TARGETS,
    },
    state,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    binary: std::path::PathBuf,
    port: u16,
}

impl Fixture {
    fn new(packaged: bool) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let binary = if packaged {
            let binary = root.path().join("prefix/bin/gridstator");
            fs::create_dir_all(binary.parent().unwrap())?;
            fs::copy(env!("CARGO_BIN_EXE_gridstator"), &binary)?;
            let _ = Command::new("strip")
                .arg("--strip-debug")
                .arg(&binary)
                .status();
            let digest = FileDigest::from_path("gridstator", &binary)?;
            let artifact = ReleaseArtifact {
                archive: FileDigest {
                    file: "gridstator-native.tar.gz".into(),
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
            let metadata = root.path().join("prefix/share/gridstator/releases/test");
            fs::create_dir_all(&metadata)?;
            fs::write(
                metadata.join("gridstator-release.json"),
                manifest.to_bytes()?,
            )?;
            binary
        } else {
            env!("CARGO_BIN_EXE_gridstator").into()
        };
        let tools = root.path().join("tools");
        fs::create_dir(&tools)?;
        let systemctl = tools.join("systemctl");
        fs::write(
            &systemctl,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SETUP_TEST_ROOT/trace"
case " $* " in
  *' show '*)
    if [ -f "$SETUP_TEST_ROOT/properties" ]; then
      /bin/cat "$SETUP_TEST_ROOT/properties"
    else
      printf 'LoadState=not-found\nActiveState=inactive\n'
    fi ;;
  *' daemon-reload '*) : ;;
  *' enable '*) /bin/cp "$SETUP_TEST_ROOT/inactive-properties" "$SETUP_TEST_ROOT/properties" ;;
  *' restart '*|*' start '*)
    if [ -f "$SETUP_TEST_ROOT/fail-first" ]; then
      /bin/rm "$SETUP_TEST_ROOT/fail-first"
      exit 7
    fi
    "$SETUP_TEST_BINARY" controller start --with-agent --data-dir "$SETUP_TEST_ROOT/controller" > "$SETUP_TEST_ROOT/controller.log" 2>&1 &
    printf '%s' "$!" > "$SETUP_TEST_ROOT/pid"
    /bin/cp "$SETUP_TEST_ROOT/active-properties" "$SETUP_TEST_ROOT/properties" ;;
  *) exit 98 ;;
esac
"#,
        )?;
        fs::set_permissions(systemctl, fs::Permissions::from_mode(0o755))?;
        let loginctl = tools.join("loginctl");
        fs::write(
            &loginctl,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SETUP_TEST_ROOT/linger-trace"
case " $* " in
  *' enable-linger '*) [ ! -f "$SETUP_TEST_ROOT/deny-linger" ] ;;
  *' show-user '*)
    if [ -f "$SETUP_TEST_ROOT/deny-linger" ]; then printf 'no\n'; else printf 'yes\n'; fi ;;
  *) exit 98 ;;
esac
"#,
        )?;
        fs::set_permissions(loginctl, fs::Permissions::from_mode(0o755))?;
        let docker = tools.join("docker");
        fs::write(&docker, "#!/bin/sh\nexit 1\n")?;
        fs::set_permissions(docker, fs::Permissions::from_mode(0o755))?;
        for (file, active) in [
            ("active-properties", "active"),
            ("inactive-properties", "inactive"),
        ] {
            fs::write(
                root.path().join(file),
                format!(
                    "LoadState=loaded\nActiveState={active}\nExecStart={{ argv[]={:?} controller start --data-dir {:?} --with-agent ; ignore_errors=no ; }}\nEnvironment=\nEnvironmentFiles=\n",
                    binary,
                    root.path().join("controller")
                ),
            )?;
        }
        Ok(Self { root, binary, port })
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .current_dir(self.root())
            .env("HOME", self.root())
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("XDG_DATA_HOME", self.root().join("data"))
            .env("GRIDSTATOR_CONTROLLER_DATA", self.root().join("controller"))
            .env("GRIDSTATOR_CONFIG", self.root().join("client.json"))
            .env("SETUP_TEST_ROOT", self.root())
            .env("SETUP_TEST_BINARY", &self.binary)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root().join("tools").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        for key in [
            "GRIDSTATOR_URL",
            "GRIDSTATOR_TOKEN",
            "GRIDSTATOR_CA_FILE",
            "GRIDSTATOR_INSTALL_ROOT",
            "GRIDSTATOR_AGENT_DATA",
            "GRIDSTATOR_SUPERVISED",
        ] {
            command.env_remove(key);
        }
        command
    }

    fn start(&self) -> Result<Child> {
        let mut child = self
            .command()
            .arg("bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let payload = json!({"version":1,"operation":"setup","configPath":self.root().join("client.json"),"port":self.port});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_string(&payload)?.as_bytes())?;
        Ok(child)
    }

    fn run(&self) -> Result<Output> {
        Ok(self.start()?.wait_with_output()?)
    }

    async fn initialize(&self) -> Result<Value> {
        gridstator::controller::initialize(
            &self.root().join("controller"),
            "127.0.0.1",
            self.port,
            &[],
            false,
            &self.root().join("client.json"),
        )
        .await
    }

    fn trace(&self) -> String {
        fs::read_to_string(self.root().join("trace")).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(self.root().join("pid"))
            && let Ok(pid) = pid.parse::<i32>()
        {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline
                && std::net::TcpListener::bind(("127.0.0.1", self.port)).is_err()
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn success(output: Output) -> Result<Value> {
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[tokio::test]
async fn fresh_setup_is_shared_across_windows_and_runs_a_worker_without_docker() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let first = fixture.start()?;
    let second = fixture.start()?;
    let first = success(first.wait_with_output()?)?;
    let second = success(second.wait_with_output()?)?;
    assert_eq!(first["status"], "connected");
    assert_eq!(first["host"]["clusterId"], second["host"]["clusterId"]);
    assert_eq!(first["workerWaitingForDocker"], true);
    assert_eq!(first["host"]["service"]["state"], "active");
    assert_eq!(first["host"]["setupPending"], false);
    assert!(first.get("persistenceWarning").is_none());
    assert_eq!(
        fixture
            .trace()
            .lines()
            .filter(|line| line.contains(" restart "))
            .count(),
        1
    );
    let saved = state::read_json(&fixture.root().join("client.json"))?;
    assert!(!first.to_string().contains(saved["token"].as_str().unwrap()));
    assert!(
        !fixture
            .root()
            .join("controller/setup-pending.json")
            .exists()
    );
    let definition = fs::read_to_string(
        fixture
            .root()
            .join(".config/systemd/user/gridstator-native-controller.service"),
    )?;
    assert!(definition.contains("--with-agent"));
    assert!(definition.contains("WantedBy=default.target"));
    let database = gridstator::db::Db::open(&fixture.root().join("controller/controller.db"))?;
    let tokens = database
        .call(|connection| gridstator::db::list(connection, "tokens"))
        .await?;
    assert_eq!(
        tokens
            .iter()
            .filter(|token| token["kind"] == "client")
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn setup_resumes_after_service_installation_fails_without_reinitializing() -> Result<()> {
    let fixture = Fixture::new(true)?;
    fs::write(fixture.root().join("fail-first"), "")?;
    let failed = fixture.run()?;
    assert!(!failed.status.success());
    let status = success(
        fixture
            .command()
            .args(["controller", "local", "status", "--json", "--data-dir"])
            .arg(fixture.root().join("controller"))
            .output()?,
    )?;
    assert_eq!(status["setupPending"], true);
    let before = fs::read(fixture.root().join("client.json"))?;
    let result = success(fixture.run()?)?;
    assert_eq!(result["status"], "connected");
    assert_eq!(result["host"]["setupPending"], false);
    assert_eq!(before, fs::read(fixture.root().join("client.json"))?);
    assert!(fixture.trace().contains("--user start "));
    Ok(())
}

#[tokio::test]
async fn stopped_existing_controller_is_left_stopped() -> Result<()> {
    let fixture = Fixture::new(false)?;
    fixture.initialize().await?;
    fs::copy(
        fixture.root().join("inactive-properties"),
        fixture.root().join("properties"),
    )?;
    let before = fs::read(fixture.root().join("client.json"))?;
    let output = fixture.run()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Start Controller"));
    assert_eq!(before, fs::read(fixture.root().join("client.json"))?);
    assert!(fixture.trace().lines().all(|line| line.contains(" show ")));
    Ok(())
}

#[tokio::test]
async fn revoked_local_login_is_not_recreated_by_setup() -> Result<()> {
    let fixture = Fixture::new(false)?;
    fixture.initialize().await?;
    let path = fixture.root().join("client.json");
    let mut saved = state::read_json(&path)?;
    saved["token"] = json!("revoked-token-must-not-appear");
    state::write_json(&path, &saved)?;
    let before = fs::read(&path)?;
    let output = fixture.run()?;
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("revoked"));
    assert!(!error.contains("revoked-token-must-not-appear"));
    assert_eq!(before, fs::read(path)?);
    assert!(fixture.trace().lines().all(|line| line.contains(" show ")));
    Ok(())
}

#[tokio::test]
async fn foreign_connection_is_preserved_and_unpackaged_binary_does_not_initialize() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let path = fixture.root().join("client.json");
    state::write_json(
        &path,
        &json!({"version":1,"url":"https://other:7300","caPem":"pinned-ca","caFingerprint":"other","token":"private-foreign-token"}),
    )?;
    let before = fs::read(&path)?;
    let output = fixture.run()?;
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("saved connection"));
    assert!(!error.contains("private-foreign-token"));
    assert_eq!(before, fs::read(&path)?);
    assert!(!fixture.root().join("controller/controller.json").exists());
    fs::remove_file(path)?;
    let output = fixture.run()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("verified native release manifest"));
    assert!(!fixture.root().join("controller/controller.json").exists());
    assert!(!fixture.root().join("client.json").exists());
    Ok(())
}

#[tokio::test]
async fn setup_recovers_only_marked_incomplete_initialization() -> Result<()> {
    for with_config in [false, true] {
        let fixture = Fixture::new(true)?;
        let directory = fixture.root().join("controller");
        state::private_dir(&directory.join("pki"))?;
        fs::write(directory.join("pki/ca.key"), "preserved-incomplete-key")?;
        if with_config {
            state::write_json(
                &directory.join("controller.json"),
                &json!({"version":2,"clusterId":"interrupted-cluster","host":"0.0.0.0","port":fixture.port}),
            )?;
        }
        let unmarked = fixture.run()?;
        assert!(!unmarked.status.success());
        assert!(String::from_utf8_lossy(&unmarked.stderr).contains("incomplete or unowned"));
        assert_eq!(
            fs::read_to_string(directory.join("pki/ca.key"))?,
            "preserved-incomplete-key"
        );
        state::write_json(
            &directory.join("setup-pending.json"),
            &json!({"initializing":true,"configPath":fixture.root().join("client.json")}),
        )?;
        let status = success(
            fixture
                .command()
                .args(["controller", "local", "status", "--json", "--data-dir"])
                .arg(&directory)
                .output()?,
        )?;
        assert_eq!(status["setupPending"], true);
        let result = success(fixture.run()?)?;
        assert_eq!(result["status"], "connected");
        let archive = fs::read_dir(&directory)?
            .filter_map(std::result::Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("setup-recovery-")
            })
            .expect("interrupted artifacts were archived")
            .path();
        assert_eq!(
            fs::read_to_string(archive.join("pki/ca.key"))?,
            "preserved-incomplete-key"
        );
        assert_ne!(result["host"]["clusterId"], "interrupted-cluster");
    }
    Ok(())
}

#[tokio::test]
async fn setup_preserves_finished_initialization_before_marker_conversion() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let initialized = fixture.initialize().await?;
    let before = fs::read(fixture.root().join("client.json"))?;
    state::write_json(
        &fixture.root().join("controller/setup-pending.json"),
        &json!({"initializing":true,"configPath":fixture.root().join("client.json")}),
    )?;
    let result = success(fixture.run()?)?;
    assert_eq!(result["host"]["clusterId"], initialized["clusterId"]);
    assert_eq!(before, fs::read(fixture.root().join("client.json"))?);
    assert_eq!(result["host"]["setupPending"], false);
    Ok(())
}

#[tokio::test]
async fn setup_repairs_missing_supervisor_after_payload_activation() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let manifest = ReleaseManifest::read(
        fixture
            .root()
            .join("prefix/share/gridstator/releases/test/gridstator-release.json"),
    )?;
    let store = gridstator::install::InstallStore::new(fixture.root().join("controller/install"));
    store.install_verified_executable(
        &manifest,
        gridstator::release::host_target()?,
        &fixture.binary,
    )?;
    store.activate(&manifest.build_id, None)?;
    assert!(!store.supervisor_path().exists());
    let result = success(fixture.run()?)?;
    assert_eq!(result["status"], "connected");
    assert!(store.supervisor_path().is_file());
    manifest
        .target(gridstator::release::host_target()?)?
        .executable
        .verify(store.supervisor_path())?;
    Ok(())
}

#[tokio::test]
async fn denied_lingering_returns_a_persistent_actionable_warning_without_blocking_setup()
-> Result<()> {
    let fixture = Fixture::new(true)?;
    fs::write(fixture.root().join("deny-linger"), "")?;
    let result = success(fixture.run()?)?;
    assert_eq!(result["status"], "connected");
    assert!(
        result["persistenceWarning"]
            .as_str()
            .unwrap()
            .contains("last SSH session")
    );
    assert_eq!(
        result["persistenceWarning"],
        result["host"]["service"]["persistenceWarning"]
    );
    assert!(
        fs::read_to_string(fixture.root().join("linger-trace"))?
            .contains("--no-ask-password enable-linger")
    );
    fs::remove_file(fixture.root().join("deny-linger"))?;
    let status = success(
        fixture
            .command()
            .args(["controller", "local", "status", "--json", "--data-dir"])
            .arg(fixture.root().join("controller"))
            .output()?,
    )?;
    assert!(status["service"].get("persistenceWarning").is_none());
    Ok(())
}
