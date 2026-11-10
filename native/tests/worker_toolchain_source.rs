//! Actual Linux replacement for the conditional v1 managed act/Node-switch scenario.
//! Node runtime management is removed; native core/launcher identity and failed candidate safety
//! are asserted explicitly alongside a real controller-mirrored act installation.
#![cfg(target_os = "linux")]

use anyhow::{Context, Result, bail, ensure};
use gridstator::{
    client::{Client, Config},
    db,
    install::InstallStore,
    release::{
        self, ArtifactMirror, FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest,
    },
    state,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};

const ACT: &str = "0.2.96";
const INVALID_ACT: &str = "9.9.99";
const WAIT: Duration = Duration::from_secs(120);

struct OwnedProcess(Child);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Some(pid) = self.0.id() {
            // Signal only the live child created by this fixture.
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
impl OwnedProcess {
    async fn stop(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_none() {
            if let Some(pid) = self.0.id() {
                ensure!(
                    unsafe { libc::kill(pid as i32, libc::SIGTERM) } == 0,
                    "owned role signal failed"
                );
            }
            tokio::time::timeout(Duration::from_secs(20), self.0.wait()).await??;
        }
        Ok(())
    }
}
struct Fixture {
    root: PathBuf,
    binary: PathBuf,
}
impl Fixture {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .current_dir(&self.root)
            .env("GRIDSTATOR_CONFIG", self.root.join("client.json"))
            .env("GRIDSTATOR_CONTROLLER_DATA", self.root.join("controller"))
            .env("GRIDSTATOR_AGENT_DATA", self.root.join("worker"))
            .env("GRIDSTATOR_CACHE", self.root.join("cache"))
            .env("GRIDSTATOR_DOCKER", "/nonexistent/toolchain-source-docker")
            .env("PATH", self.root.join("empty-tools"))
            .env("NO_COLOR", "1");
        for name in [
            "GRIDSTATOR_URL",
            "GRIDSTATOR_TOKEN",
            "GRIDSTATOR_CA_FILE",
            "GRIDSTATOR_SUPERVISED",
            "GRIDSTATOR_INSTALL_ROOT",
            "GRIDSTATOR_INSTALL_VERSION",
            "GRIDSTATOR_INSTALL_TARGET",
            "GRIDSTATOR_BUILD_ID",
            "GRIDSTATOR_ACT",
        ] {
            command.env_remove(name);
        }
        command
    }
    async fn run(&self, args: &[&str]) -> Result<()> {
        let output =
            tokio::time::timeout(Duration::from_secs(30), self.command().args(args).output())
                .await??;
        ensure!(
            output.status.success(),
            "native command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
    fn start(&self, args: &[&str], log: &str) -> Result<OwnedProcess> {
        let log = fs::File::create(self.root.join(log))?;
        Ok(OwnedProcess(
            self.command()
                .args(args)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?,
        ))
    }
}
fn parser_header(target: &str) -> Vec<u8> {
    let mut bytes = vec![0; 64];
    if target.starts_with("linux/") {
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(
            &(if target.ends_with("amd64") {
                62u16
            } else {
                183u16
            })
            .to_le_bytes(),
        );
    } else {
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[4..8].copy_from_slice(
            &(if target.ends_with("amd64") {
                0x0100_0007u32
            } else {
                0x0100_000cu32
            })
            .to_le_bytes(),
        );
    }
    bytes
}
fn prepare_core(fixture: &Fixture) -> Result<(InstallStore, ReleaseManifest)> {
    let mut artifacts = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for target in release::SUPPORTED_TARGETS {
        let filename = format!("core-{}.bin", target.replace('/', "-"));
        let source = if target == release::host_target()? {
            fixture.binary.clone()
        } else {
            // Non-host members exist solely to satisfy the four-target mirror contract.
            // They are parser fixtures and are never installed or executed by this test.
            let path = fixture.root.join(&filename);
            fs::write(&path, parser_header(target))?;
            path
        };
        let archive = FileDigest::from_path(filename, &source)?;
        artifacts.insert(
            target.into(),
            ReleaseArtifact {
                executable: FileDigest {
                    file: "gridstator".into(),
                    ..archive.clone()
                },
                archive,
            },
        );
        sources.insert(target, source);
    }
    let manifest = ReleaseManifest {
        schema_version: 1,
        build_id: gridstator::BUILD_ID.into(),
        version: gridstator::VERSION.into(),
        source_revision: gridstator::SOURCE_REVISION.into(),
        protocol: ProtocolCompatibility::default(),
        artifacts,
    };
    let mirror = ArtifactMirror::new(fixture.root.join("controller/native-release-mirror"));
    mirror.cache_manifest(&manifest.to_bytes()?, &manifest.sha256()?)?;
    for target in release::SUPPORTED_TARGETS {
        mirror.cache_artifact(&manifest, target, &sources[target])?;
    }
    let store = InstallStore::open(fixture.root.join("worker-install"))?;
    store.install_verified_executable(&manifest, release::host_target()?, &fixture.binary)?;
    store.activate(gridstator::BUILD_ID, None)?;
    fs::write(
        store.supervisor_path(),
        b"old stable launcher; never execute these fixture bytes",
    )?;
    Ok((store, manifest))
}
async fn node(client: &Client, id: &str, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
    tokio::time::timeout(WAIT, async {
        loop {
            let value = client.get(&format!("/v1/nodes/{id}")).await?;
            if predicate(&value) {
                return Ok::<_, anyhow::Error>(value);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("worker did not report expected native tool state")?
}
async fn updates(client: &Client, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
    tokio::time::timeout(WAIT, async {
        loop {
            let value = client.get("/v1/updates").await?;
            if predicate(&value) {
                return Ok::<_, anyhow::Error>(value);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("controller did not report expected tool/rollout state")?
}
fn payload_pid(supervisor: u32, expected: &Path) -> Result<u32> {
    let children = fs::read_to_string(format!("/proc/{supervisor}/task/{supervisor}/children"))?;
    let expected = fs::canonicalize(expected)?;
    for child in children.split_whitespace() {
        let pid: u32 = child.parse()?;
        if fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == expected) {
            return Ok(pid);
        }
    }
    bail!("owned supervisor has no live expected native payload")
}
fn invalid_archive(path: &Path) -> Result<()> {
    let encoder =
        flate2::write::GzEncoder::new(fs::File::create(path)?, flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let bytes = [0u8; 64];
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o755);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    tar.append_data(&mut header, "act", bytes.as_slice())?;
    tar.into_inner()?.finish()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires upstream act release access; actual verified managed native worker; no Docker/services"]
async fn source_003_real_managed_act_reprobes_and_invalid_candidate_reports_without_restart_loop()
-> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("gridstator-worker-toolchain-source-")
        .tempdir()?
        .keep();
    state::private_dir(&root)?;
    let fixture = Fixture {
        binary: root.join("gridstator"),
        root,
    };
    fs::copy(env!("CARGO_BIN_EXE_gridstator"), &fixture.binary)?;
    // Core roles and managed tool probes use absolute paths. Start without a host act/Node
    // executable on PATH, matching the original missing-tool fixture rather than relying on it.
    fs::create_dir(fixture.root.join("empty-tools"))?;
    eprintln!(
        "Worker toolchain source private evidence: {}",
        fixture.root.display()
    );
    fixture
        .run(&["controller", "init", "--host", "127.0.0.1", "--port", "0"])
        .await?;
    let (store, manifest) = prepare_core(&fixture)?;
    let database = db::Db::open(&fixture.root.join("controller/controller.db"))?;
    database.write(|connection|db::put(connection,"settings","cluster",&json!({"jobRetentionDays":90,"logRetentionDays":30,"snapshotRetentionDays":7,"autoUpdate":"off","updateWindow":null,"toolchain":{"act":"off"}}))).await?;
    let mut controller = fixture.start(&["controller", "start"], "controller.log")?;
    let client = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(status) = controller.0.try_wait()? {
                bail!("private controller exited {status}");
            }
            if let Ok(runtime) = state::read_json(&fixture.root.join("controller/runtime.json")) {
                let mut config: Config =
                    serde_json::from_slice(&fs::read(fixture.root.join("client.json"))?)?;
                config.url = format!(
                    "https://127.0.0.1:{}",
                    runtime["port"].as_u64().context("bound controller port")?
                );
                let client = Client::new(config)?;
                if client.get("/v1/health").await.is_ok() {
                    return Ok::<_, anyhow::Error>(client);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    let join = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"private-toolchain-source","maxUses":1}),
        )
        .await?;
    fixture
        .run(&[
            "agent",
            "join",
            &client.config.url,
            "--token",
            join["token"].as_str().unwrap(),
            "--fingerprint",
            &client.config.ca_fingerprint,
            "--name",
            "native-act-source",
        ])
        .await?;
    let saved = state::read_json(&fixture.root.join("worker/agent.json"))?;
    let id = saved["nodeId"]
        .as_str()
        .context("worker node ID")?
        .to_owned();
    let mut worker = fixture.start(
        &[
            "supervise",
            "--install-root",
            store.root().to_str().unwrap(),
            "--",
            "agent",
            "start",
            "--data-dir",
            fixture.root.join("worker").to_str().unwrap(),
        ],
        "worker.log",
    )?;
    let baseline = node(&client, &id, |node| {
        node["status"] == "online" && node["info"]["build"]["managed"] == true
    })
    .await?;
    ensure!(
        baseline["info"]["docker"]["available"] == false,
        "Docker must remain unavailable in this proof"
    );
    ensure!(
        baseline["info"]["act"]["available"] == false
            && baseline["info"]["build"]["tools"]["act"].is_null(),
        "act must initially be unavailable on the private role PATH"
    );
    ensure!(
        baseline["info"]["build"]["tools"]["managed"] == json!([]),
        "baseline tool must not be managed yet"
    );
    let supervisor_pid = worker.0.id().context("owned supervisor PID")?;
    let payload = store.executable(gridstator::BUILD_ID)?;
    let initial_pid = payload_pid(supervisor_pid, &payload)?;
    client
        .request(
            reqwest::Method::PATCH,
            "/v1/settings",
            Some(json!({"toolchain":{"act":ACT}})),
        )
        .await?;
    updates(&client, |view| {
        view["toolchain"][0]["platforms"]
            .as_array()
            .is_some_and(|platforms| {
                platforms.iter().any(|entry| {
                    entry["platform"] == release::host_target().unwrap()
                        && entry["state"] == "ready"
                })
            })
    })
    .await?;
    let drift = node(&client, &id, |node| {
        node["update"]["state"] == "behind"
            && node["update"]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("toolchain") && detail.contains(ACT))
    })
    .await?;
    client.post("/v1/updates", json!({"nodes":[id]})).await?;
    let installed = node(&client, &id, |node| {
        node["status"] == "online"
            && node["info"]["build"]["tools"]["act"] == ACT
            && node["info"]["build"]["tools"]["managed"] == json!(["act"])
    })
    .await?;
    let completed = updates(&client, |view| view["rollout"]["state"] == "finished").await?;
    ensure!(
        payload_pid(supervisor_pid, &payload)? == initial_pid,
        "act-only update restarted the core OS process"
    );
    ensure!(
        installed["info"]["build"]["runtime"] == "rust"
            && installed["info"]["build"]["id"] == gridstator::BUILD_ID,
        "managed native core identity changed"
    );
    ensure!(
        installed["info"]["build"]["sha256"]
            == manifest.target(release::host_target()?)?.executable.sha256,
        "worker did not report the verified native executable digest"
    );
    let core_after_act = store
        .state()?
        .context("managed core activation after act")?;
    ensure!(
        core_after_act.current == gridstator::BUILD_ID
            && core_after_act.pending.is_none()
            && core_after_act.last_rollback.is_none(),
        "act-only installation changed native core activation"
    );
    ensure!(
        installed["info"]["build"].get("node").is_none(),
        "removed Node runtime identity leaked into native metadata"
    );
    ensure!(
        completed["toolchain"]
            .as_array()
            .context("toolchain view")?
            .iter()
            .all(|tool| tool["name"] == "act"),
        "native toolchain includes removed Node management"
    );
    let installed_config = state::read_json(&fixture.root.join("worker/agent.json"))?;
    let act_path = PathBuf::from(
        installed_config["managedActPath"]
            .as_str()
            .context("managed act path")?,
    );
    let executable_digest = release::hash_file(&act_path)?.1;
    ensure!(
        installed_config["managedActSha256"] == executable_digest,
        "managed executable digest differs from persisted identity"
    );
    let version = Command::new(&act_path).arg("--version").output().await?;
    ensure!(
        version.status.success() && String::from_utf8_lossy(&version.stdout).contains(ACT),
        "installed act did not execute as 0.2.96"
    );
    let target = release::host_target()?;
    let key = format!("act-{ACT}-{}", target.replace('/', "-"));
    let mirror = database
        .call(move |connection| {
            db::get(connection, "toolchain", &key)?.context("actual managed act mirror record")
        })
        .await?;
    let checksums = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .get(format!(
            "https://github.com/nektos/act/releases/download/v{ACT}/checksums.txt"
        ))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let filename = if target == "linux/amd64" {
        "act_Linux_x86_64.tar.gz"
    } else {
        "act_Linux_arm64.tar.gz"
    };
    let expected = release::checksum_for(&checksums, filename)?;
    ensure!(
        mirror["sha256"] == expected
            && release::hash_file(mirror["path"].as_str().context("act archive path")?)?.1
                == expected,
        "controller archive differs from independent upstream checksums"
    );
    let bad = fixture.root.join("synthetic-invalid-act.tar.gz");
    invalid_archive(&bad)?;
    let (size, digest) = release::hash_file(&bad)?;
    let record = format!("act-{INVALID_ACT}-{}", target.replace('/', "-"));
    let row = json!({"state":"ready","path":bad,"sha256":digest,"size":size,"reason":null});
    database
        .write(move |connection| db::put(connection, "toolchain", &record, &row))
        .await?;
    client
        .request(
            reqwest::Method::PATCH,
            "/v1/settings",
            Some(json!({"toolchain":{"act":INVALID_ACT}})),
        )
        .await?;
    let mut failures = vec![];
    for _ in 0..3 {
        client.post("/v1/updates", json!({"nodes":[id]})).await?;
        let halted = updates(&client, |view| view["rollout"]["state"] == "halted").await?;
        ensure!(
            halted["rollout"]["haltReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("executable target unsupported")),
            "invalid candidate failure was not reported: {halted}"
        );
        ensure!(
            payload_pid(supervisor_pid, &payload)? == initial_pid,
            "failed verified tool candidate restarted the native core"
        );
        let current = node(&client, &id, |node| {
            node["status"] == "online" && node["update"]["state"] == "failed"
        })
        .await?;
        ensure!(
            current["info"]["build"]["tools"]["act"] == ACT,
            "failed candidate replaced last working managed act"
        );
        ensure!(
            current["settings"]["schedulingMode"] == "enabled",
            "failed update did not restore scheduling"
        );
        let state = store.state()?.context("managed install state")?;
        ensure!(
            state.current == gridstator::BUILD_ID
                && state.pending.is_none()
                && state.last_rollback.is_none(),
            "tool-only failure mutated the native core activation"
        );
        ensure!(
            release::hash_file(&act_path)?.1 == executable_digest,
            "failed candidate modified working act bytes"
        );
        let config = state::read_json(&fixture.root.join("worker/agent.json"))?;
        ensure!(
            config["managedActVersion"] == ACT && config["managedActSha256"] == executable_digest,
            "failed candidate changed managed tool config"
        );
        ensure!(
            !fixture
                .root
                .join("worker/toolchain/act")
                .join(INVALID_ACT)
                .join(target.replace('/', "-"))
                .join("act")
                .exists(),
            "invalid executable became installed"
        );
        failures.push(halted);
    }
    manifest
        .target(target)?
        .executable
        .verify(store.supervisor_path())?;
    let final_node = node(&client, &id, |node| node["status"] == "online").await?;
    let report = json!({
        "scope":"Actual Linux managed worker, real upstream act 0.2.96 via private controller; three digest-correct synthetic invalid executable candidates fail without an OS process restart; no Docker/services",
        "target":target,"coreManifest":manifest,"supervisorPid":supervisor_pid,
        "initialPayloadPid":initial_pid,"finalPayloadPid":payload_pid(supervisor_pid,&payload)?,
        "baseline":baseline,"toolchainDrift":drift,"installed":installed,
        "coreAfterAct":core_after_act,"completed":completed,"archiveSha256":expected,
        "actExecutableSha256":executable_digest,
        "reportedActVersion":String::from_utf8_lossy(&version.stdout).trim(),
        "syntheticInvalidArchive":{"version":INVALID_ACT,"sha256":digest,"size":size,"contents":"64 zero bytes named act; intentionally not an executable"},
        "failureChecks":failures,"finalNode":final_node,"finalInstall":store.state()?
    });
    worker.stop().await?;
    controller.stop().await?;
    drop(database);
    if let Some(destination) = std::env::var_os("GRIDSTATOR_TOOLCHAIN_SOURCE_EVIDENCE") {
        let destination = PathBuf::from(destination);
        state::private_dir(&destination)?;
        state::write_json(&destination.join("worker-toolchain-source.json"), &report)?;
        fs::write(destination.join("upstream-checksums.txt"), checksums)?;
        let credential = regex::Regex::new(r"gs[acj]_[A-Za-z0-9_-]+")?;
        for log in ["worker.log", "controller.log"] {
            let bytes = fs::read(fixture.root.join(log))?;
            ensure!(
                !credential.is_match(&String::from_utf8_lossy(&bytes)),
                "credential-like token in source log"
            );
            fs::write(destination.join(log), bytes)?;
        }
    }
    eprintln!(
        "Actual managed act 0.2.96 installed/reprobed; three invalid candidate reports preserved core PID {initial_pid}, working tool and activation state"
    );
    if std::env::var_os("GRIDSTATOR_TOOLCHAIN_SOURCE_KEEP").is_none() {
        fs::remove_dir_all(&fixture.root)?;
    }
    Ok(())
}
