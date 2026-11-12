//! Actual Linux installer and controller schema rollback rehearsal.
//! Opt in: cargo test --test recovery_e2e -- --ignored --nocapture
//! Builds real native payloads in a disposable source copy. Non-host bundle entries are explicitly
//! header-only parsing fixtures; this test is never evidence for their runtime/signing gates.
#![cfg(target_os = "linux")]
use anyhow::{Context, Result, ensure};
use flate2::{Compression, write::GzEncoder};
use gridstator::{
    client::{Client, Config},
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
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn executable(name: &str) -> Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").context("PATH")?)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .with_context(|| format!("build-host prerequisite missing: {name}"))
}
fn command_ok(command: &mut Command) -> Result<std::process::Output> {
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}
fn metadata(binary: &Path) -> Result<Value> {
    let output = command_ok(Command::new(binary).args(["build-info", "--json"]))?;
    Ok(serde_json::from_slice(&output.stdout)?)
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        } else if entry.file_type()?.is_file() {
            fs::copy(entry.path(), destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}
fn source_hashes(root: &Path, path: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            source_hashes(root, &entry.path(), files)?;
        } else if entry.file_type()?.is_file() {
            files.insert(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                release::hash_file(entry.path())?.1,
            );
        }
    }
    Ok(())
}
fn source_copy(root: &Path) -> Result<PathBuf> {
    let original = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let destination = root.join("source/native");
    fs::create_dir_all(&destination)?;
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "rust-toolchain.toml",
    ] {
        fs::copy(original.join(name), destination.join(name))?;
    }
    for name in ["src", "contracts"] {
        copy_tree(&original.join(name), &destination.join(name))?;
    }
    fs::copy(original.join("../LICENSE"), root.join("source/LICENSE"))?;
    fs::create_dir_all(root.join("source/scripts"))?;
    fs::copy(
        original.join("../scripts/install-native.sh"),
        root.join("source/scripts/install-native.sh"),
    )?;

    Ok(destination)
}
fn build(source: &Path, root: &Path, version: &str, build_id: &str, bad: bool) -> Result<PathBuf> {
    let old: Value = serde_json::from_str(
        &fs::read_to_string(source.join("build-version.json"))
            .unwrap_or_else(|_| json!({"version":gridstator::VERSION}).to_string()),
    )?;
    let old = old["version"].as_str().context("old fixture version")?;
    for name in ["Cargo.toml", "Cargo.lock"] {
        let path = source.join(name);
        let text = fs::read_to_string(&path)?;
        let (from, to) = if name == "Cargo.lock" {
            (
                format!("name = \"gridstator\"\nversion = \"{old}\""),
                format!("name = \"gridstator\"\nversion = \"{version}\""),
            )
        } else {
            (
                format!("version = \"{old}\""),
                format!("version = \"{version}\""),
            )
        };
        ensure!(
            text.contains(&from),
            "fixture version stamp missing in {name}"
        );
        fs::write(path, text.replacen(&from, &to, 1))?;
    }
    fs::write(
        source.join("build-version.json"),
        json!({"version":version}).to_string(),
    )?;
    if bad {
        fs::write(
            source.join("src/main.rs"),
            r#"#[tokio::main]
async fn main(){
 let args=std::env::args().collect::<Vec<_>>();
 if args.get(1).is_some_and(|v|v=="controller")&&args.get(2).is_some_and(|v|v=="start"){
  let index=args.iter().position(|v|v=="--data-dir").expect("owned fixture data directory");let directory=std::path::PathBuf::from(&args[index+1]);
  let connection=rusqlite::Connection::open(directory.join("controller.db")).unwrap();
  let column:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('records') WHERE name='candidate_only')",[],|row|row.get(0)).unwrap();
  if !column{connection.execute_batch("ALTER TABLE records ADD COLUMN candidate_only TEXT").unwrap();}
  connection.execute_batch("PRAGMA user_version=999; CREATE TABLE IF NOT EXISTS candidate_schema(marker TEXT); INSERT INTO candidate_schema VALUES('failed-candidate'); DELETE FROM records WHERE kind='jobs'; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
  std::fs::write(directory.join("candidate-mutated-schema"),gridstator::BUILD_ID).unwrap();
  eprintln!("deliberate rehearsal candidate changed schema and exits before controller health");std::process::exit(78);
 }
 std::process::exit(gridstator::cli::run().await);
}
"#,
        )?;
    }
    let mut files = BTreeMap::new();
    source_hashes(source, &source.join("src"), &mut files)?;
    for name in ["Cargo.toml", "Cargo.lock", "build.rs"] {
        files.insert(name.to_owned(), release::hash_file(source.join(name))?.1);
    }
    for name in ["../LICENSE", "../scripts/install-native.sh"] {
        files.insert(name.to_owned(), release::hash_file(source.join(name))?.1);
    }
    fs::write(
        root.join(format!("{build_id}-source-sha256.json")),
        serde_json::to_vec_pretty(
            &json!({"buildId":build_id,"version":version,"schemaCrashEntrypoint":bad,"files":files}),
        )?,
    )?;
    let target = match release::host_target()? {
        "linux/amd64" => "x86_64-unknown-linux-musl",
        "linux/arm64" => "aarch64-unknown-linux-musl",
        _ => unreachable!(),
    };
    let log = fs::File::create(root.join(format!("build-{build_id}.log")))?;
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or(executable("cargo")?);
    eprintln!(
        "Building actual {build_id} ({version}) for {target}; log in {}",
        root.display()
    );
    let status = Command::new(cargo)
        .current_dir(source)
        .args([
            "build",
            "--offline",
            "--locked",
            "--bin",
            "gridstator",
            "--target",
            target,
        ])
        .env(
            "CARGO_TARGET_DIR",
            std::env::var_os("GRIDSTATOR_RECOVERY_BUILD_CACHE")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("build-cache")),
        )
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CFLAGS", "-fno-sanitize=all")
        .env("CARGO_BUILD_JOBS", "2")
        .env(
            "CC",
            std::env::var_os("GRIDSTATOR_REHEARSAL_CC").unwrap_or_else(|| "musl-gcc".into()),
        )
        .env("GRIDSTATOR_BUILD_ID", build_id)
        .env("GRIDSTATOR_SOURCE_REVISION", gridstator::SOURCE_REVISION)
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()?;
    ensure!(
        status.success(),
        "fixture build failed; inspect build-{build_id}.log"
    );
    let binary = root.join(format!("{build_id}-gridstator"));
    fs::copy(
        std::env::var_os("GRIDSTATOR_RECOVERY_BUILD_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("build-cache"))
            .join(target)
            .join("debug/gridstator"),
        &binary,
    )?;
    let info = metadata(&binary)?;
    ensure!(
        info["buildId"] == build_id && info["version"] == version,
        "compiled identity mismatch"
    );
    let linkage = command_ok(Command::new("readelf").args(["-l", "-d"]).arg(&binary))?;
    let linkage = String::from_utf8(linkage.stdout)?;
    ensure!(
        !linkage.contains("INTERP") && !linkage.contains("NEEDED"),
        "rehearsal distribution must be static"
    );
    fs::write(
        root.join(format!("{build_id}-build-info.json")),
        serde_json::to_vec_pretty(&info)?,
    )?;
    Ok(binary)
}
fn header(target: &str) -> Vec<u8> {
    let mut bytes = vec![0; 64];
    if target.starts_with("linux/") {
        bytes[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        let machine: u16 = if target.ends_with("amd64") { 62 } else { 183 };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    } else {
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        let cpu: u32 = if target.ends_with("amd64") {
            0x0100_0007
        } else {
            0x0100_000c
        };
        bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
    }
    bytes
}
fn archive(path: &Path, files: &[(&str, PathBuf)]) -> Result<()> {
    let gzip = GzEncoder::new(fs::File::create(path)?, Compression::fast());
    let mut tar = tar::Builder::new(gzip);
    for (name, file) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(fs::metadata(file)?.len());
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, *name, fs::File::open(file)?)?;
    }
    tar.into_inner()?.finish()?.sync_all()?;
    Ok(())
}
struct Package {
    directory: PathBuf,
    manifest: ReleaseManifest,
    bundle: PathBuf,
}
fn package(root: &Path, binary: &Path) -> Result<Package> {
    let info = metadata(binary)?;
    let directory = root.join(info["buildId"].as_str().unwrap());
    fs::create_dir(&directory)?;
    let mut artifacts = BTreeMap::new();
    for target in release::SUPPORTED_TARGETS {
        let payload = if target == release::host_target()? {
            binary.to_owned()
        } else {
            let path = directory.join(format!("NOT-RUNTIME-PROOF-{}", target.replace('/', "-")));
            fs::write(&path, header(target))?;
            path
        };
        let name = format!(
            "gridstator-{}-{}.tar.gz",
            info["version"].as_str().unwrap(),
            target.replace('/', "-")
        );
        let path = directory.join(&name);
        archive(&path, &[("gridstator", payload.clone())])?;
        artifacts.insert(
            target.into(),
            ReleaseArtifact {
                archive: FileDigest::from_path(name, &path)?,
                executable: FileDigest::from_path("gridstator", payload)?,
            },
        );
    }
    let manifest = ReleaseManifest {
        schema_version: 1,
        build_id: info["buildId"].as_str().unwrap().into(),
        version: info["version"].as_str().unwrap().into(),
        source_revision: info["sourceRevision"].as_str().unwrap().into(),
        protocol: ProtocolCompatibility::default(),
        artifacts,
    };
    let path = directory.join(release::RELEASE_MANIFEST_NAME);
    fs::write(&path, manifest.to_bytes()?)?;
    let bundle = directory.join(format!(
        "gridstator-{}-native-bundle.tar.gz",
        manifest.version
    ));
    let mut members = vec![(release::RELEASE_MANIFEST_NAME, path.clone())];
    for artifact in manifest.artifacts.values() {
        members.push((
            artifact.archive.file.as_str(),
            directory.join(&artifact.archive.file),
        ));
    }
    archive(&bundle, &members)?;
    let mut sums = format!(
        "{}  {}\n",
        release::hash_file(&path)?.1,
        release::RELEASE_MANIFEST_NAME
    );
    for artifact in manifest.artifacts.values() {
        sums.push_str(&format!(
            "{}  {}\n",
            artifact.archive.sha256, artifact.archive.file
        ));
    }
    let installer = directory.join("install-native.sh");
    fs::write(
        &installer,
        include_bytes!("../../scripts/install-native.sh"),
    )?;
    sums.push_str(&format!(
        "{}  install-native.sh\n",
        release::hash_file(&installer)?.1
    ));
    fs::write(directory.join("SHA256SUMS"), sums)?;
    Ok(Package {
        directory,
        manifest,
        bundle,
    })
}
async fn distribution(package: &Package) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(&package.directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.insert(
                format!(
                    "/v{}/{}",
                    package.manifest.version,
                    entry.file_name().to_string_lossy()
                ),
                fs::read(entry.path())?,
            );
        }
    }
    let files = std::sync::Arc::new(files);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let files = files.clone();
            tokio::spawn(async move {
                let mut bytes = vec![];
                let mut buffer = [0; 1024];
                while bytes.len() < 8192 {
                    let count = socket.read(&mut buffer).await?;
                    if count == 0 {
                        return Ok::<(), std::io::Error>(());
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let (status, body) = files
                    .get(path)
                    .map(|body| ("200 OK", body.as_slice()))
                    .unwrap_or(("404 Not Found", b"missing"));
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await?;
                socket.write_all(body).await?;
                Ok::<(), std::io::Error>(())
            });
        }
    });
    Ok((format!("http://{address}"), server))
}
fn tool_path(root: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::symlink;
    let directory = root.join("target-os-tools-without-node");
    fs::create_dir(&directory)?;
    for name in [
        "curl",
        "tar",
        "gzip",
        "awk",
        "od",
        "tr",
        "mktemp",
        "uname",
        "wc",
        "cp",
        "chmod",
        "mv",
        "mkdir",
        "rm",
        "rmdir",
        "sha256sum",
        "cat",
    ] {
        symlink(executable(name)?, directory.join(name))?;
    }
    Ok(directory)
}
struct OwnedChild(Child);
impl OwnedChild {
    fn stop(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_none() {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            for _ in 0..300 {
                if self.0.try_wait()?.is_some() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            self.0.kill()?;
            self.0.wait()?;
        }
        Ok(())
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
struct OwnedService {
    manager: gridstator::services::ServiceManager,
    config: gridstator::services::ServiceConfig,
}
impl Drop for OwnedService {
    fn drop(&mut self) {
        let _ = self.manager.uninstall(&self.config);
        let _ = Command::new("systemctl")
            .args(["--user", "reset-failed", &self.config.unit_name()])
            .output();
    }
}
enum NativeRole {
    Foreground(OwnedChild),
    Systemd(Box<OwnedService>),
}
impl NativeRole {
    fn stop(&mut self, root: &Path) -> Result<()> {
        match self {
            Self::Foreground(child) => child.stop(),
            Self::Systemd(service) => {
                service
                    .manager
                    .control(&service.config, gridstator::services::ServiceAction::Stop)?;
                let output = command_ok(Command::new("journalctl").args([
                    "--user",
                    "--unit",
                    &service.config.unit_name(),
                    "--no-pager",
                    "--output=short-iso",
                ]))?;
                fs::write(root.join("supervisor.log"), output.stdout)?;
                Ok(())
            }
        }
    }
    fn name(&self) -> Option<String> {
        match self {
            Self::Foreground(_) => None,
            Self::Systemd(service) => Some(service.config.unit_name()),
        }
    }
}
fn native_role(
    root: &Path,
    store: &InstallStore,
    directory: &Path,
    login: &Path,
    home: &Path,
) -> Result<NativeRole> {
    if std::env::var_os("GRIDSTATOR_RECOVERY_SYSTEMD").is_some() {
        command_ok(Command::new("systemctl").args(["--user", "show-environment"]))?;
        let mut config = gridstator::services::ServiceConfig::new(
            gridstator::services::ServiceRole::Controller,
            store.supervisor_path(),
            store.root().to_owned(),
            directory.to_owned(),
            dirs::home_dir().context("service account home")?,
            unsafe { libc::geteuid() },
        );
        config.name = format!("gridstator-native-cutover-{}", state::id().to_lowercase());
        config.extra_args = vec![
            "--with-agent".into(),
            "--agent-name".into(),
            "rehearsal-embedded".into(),
        ];
        config.path_dirs = vec![root.join("target-os-tools-without-node")];
        config.environment.insert(
            "GRIDSTATOR_CONFIG".into(),
            login.to_string_lossy().into_owned(),
        );
        let service = OwnedService {
            manager: gridstator::services::ServiceManager::new(
                gridstator::services::ServicePlatform::Systemd,
            ),
            config,
        };
        fs::write(
            root.join("native-unit.service"),
            service.manager.render(&service.config)?,
        )?;
        service.manager.install(&service.config, false)?;
        Ok(NativeRole::Systemd(Box::new(service)))
    } else {
        Ok(NativeRole::Foreground(launch(
            Command::new(store.supervisor_path())
                .args(["supervise", "--install-root"])
                .arg(store.root())
                .args([
                    "--",
                    "controller",
                    "start",
                    "--with-agent",
                    "--agent-name",
                    "rehearsal-embedded",
                    "--data-dir",
                ])
                .arg(directory)
                .env("GRIDSTATOR_CONFIG", login)
                .env("HOME", home)
                .env("PATH", root.join("target-os-tools-without-node")),
            &root.join("supervisor.log"),
        )?))
    }
}
fn launch(command: &mut Command, log: &Path) -> Result<OwnedChild> {
    let output = fs::File::create(log)?;
    Ok(OwnedChild(
        command
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .spawn()?,
    ))
}
async fn client_for(directory: &Path, config: &Path, build: Option<&str>) -> Result<Client> {
    let mut config: Config = serde_json::from_slice(&fs::read(config)?)?;
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if let Ok(runtime) = state::read_json(&directory.join("runtime.json")) {
                config.url = format!("https://127.0.0.1:{}", runtime["port"].as_u64().unwrap());
            }
            let client = Client::new(config.clone())?;
            if let Ok(cluster) = client.get("/v1/cluster").await
                && build.is_none_or(|wanted| cluster["build"]["id"] == wanted)
            {
                return Ok::<_, anyhow::Error>(client);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await?
}
async fn embedded_ready(client: &Client, build: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let nodes = client.get("/v1/nodes").await?;
            if nodes["nodes"].as_array().is_some_and(|nodes| {
                nodes.iter().any(|node| {
                    node["name"] == "rehearsal-embedded"
                        && node["status"] == "online"
                        && node["info"]["build"]["id"] == build
                        && node["settings"]["schedulingMode"] == "enabled"
                        && node["info"]["docker"]["available"] == false
                })
            }) {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await?
}
async fn publish(client: &Client, package: &Package) -> Result<()> {
    let hash = release::hash_file(&package.bundle)?.1;
    let response = client
        .http
        .post(format!(
            "{}/v1/controller/build?sha256={hash}",
            client.config.url
        ))
        .bearer_auth(&client.config.token)
        .header("content-type", "application/gzip")
        .body(fs::read(&package.bundle)?)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    ensure!(status.is_success(), "publish failed {status}: {body}");
    Ok(())
}
fn db_facts(path: &Path) -> Result<Value> {
    let connection = rusqlite::Connection::open(path)?;
    let schema: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let column: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('records') WHERE name='candidate_only')",
        [],
        |row| row.get(0),
    )?;
    let candidate_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='candidate_schema')",
        [],
        |row| row.get(0),
    )?;
    let jobs: i64 = connection.query_row(
        "SELECT COUNT(*) FROM records WHERE kind='jobs'",
        [],
        |row| row.get(0),
    )?;
    Ok(
        json!({"userVersion":schema,"candidateColumn":column,"candidateTable":candidate_table,"jobs":jobs}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "builds real static candidates and starts only disposable loopback clusters; requires Rust musl tools"]
async fn verified_install_successful_upgrade_and_schema_rollback() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temporary = tempfile::Builder::new()
        .prefix("gridstator-recovery-")
        .tempdir()?;
    let keep = std::env::var_os("GRIDSTATOR_KEEP_RECOVERY_EVIDENCE").is_some();
    let root = if keep {
        temporary.keep()
    } else {
        temporary.path().to_owned()
    };
    eprintln!("Recovery rehearsal evidence: {}", root.display());
    state::private_dir(&root)?;
    let prebuilt = std::env::var_os("GRIDSTATOR_RECOVERY_PREBUILT").map(PathBuf::from);
    let payload_source = prebuilt
        .as_ref()
        .map(|path| path.join("source/native"))
        .unwrap_or_else(|| root.join("source/native"));
    let mut payloads = vec![];
    if let Some(prebuilt) = &prebuilt {
        for id in [
            "rehearsal-baseline",
            "rehearsal-confirmed",
            "rehearsal-schema-crash",
        ] {
            let binary = root.join(format!("{id}-gridstator"));
            fs::copy(prebuilt.join(format!("{id}-gridstator")), &binary)?;
            let expected: Value =
                serde_json::from_slice(&fs::read(prebuilt.join(format!("{id}-build-info.json")))?)?;
            let actual = metadata(&binary)?;
            ensure!(
                actual == expected,
                "prebuilt executable differs from its recorded identity"
            );
            release::verify_executable_target(&binary, release::host_target()?)?;
            for suffix in ["build-info.json", "source-sha256.json"] {
                fs::copy(
                    prebuilt.join(format!("{id}-{suffix}")),
                    root.join(format!("{id}-{suffix}")),
                )?;
            }
            payloads.push(binary);
        }
    } else {
        let source = source_copy(&root)?;
        if let Some(cache) = std::env::var_os("GRIDSTATOR_RECOVERY_BUILD_CACHE") {
            state::private_dir(Path::new(&cache))?;
        }
        if std::env::var_os("GRIDSTATOR_REHEARSAL_CC").is_none() {
            executable("musl-gcc")?;
        }
        for (version, id, bad) in [
            ("2.0.0-rehearsal.1", "rehearsal-baseline", false),
            ("2.0.0-rehearsal.2", "rehearsal-confirmed", false),
            ("2.0.0-rehearsal.3", "rehearsal-schema-crash", true),
        ] {
            payloads.push(build(&source, &root, version, id, bad)?);
        }
    }
    let base = payloads.remove(0);
    let good = payloads.remove(0);
    let bad = payloads.remove(0);
    let base = package(&root, &base)?;
    let good = package(&root, &good)?;
    let bad = package(&root, &bad)?;
    let tools = tool_path(&root)?;
    command_ok(Command::new("/bin/sh").args(["-c",r#"for tool in node npm rustc cargo python3 jq; do if command -v "$tool" >/dev/null 2>&1; then exit 1; fi; done"#]).env_clear().env("PATH",&tools))?;
    let home = root.join("clean-home");
    state::private_dir(&home)?;
    let prefix = root.join("installer prefix");
    let (url, download) = distribution(&base).await?;
    let installer = root.join("downloaded-install-native.sh");
    let downloaded_sums = root.join("downloaded-SHA256SUMS");
    for (name, destination) in [
        ("install-native.sh", &installer),
        ("SHA256SUMS", &downloaded_sums),
    ] {
        command_ok(
            Command::new(tools.join("curl"))
                .args(["--fail", "--silent", "--show-error", "--output"])
                .arg(destination)
                .arg(format!("{url}/v{}/{name}", base.manifest.version))
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &tools),
        )?;
    }
    ensure!(
        release::hash_file(&installer)?.1
            == release::checksum_for(&fs::read_to_string(downloaded_sums)?, "install-native.sh")?,
        "downloaded installer checksum mismatch"
    );
    let output = command_ok(
        Command::new("/bin/sh")
            .arg(installer)
            .args(["--tag", &format!("v{}", base.manifest.version), "--prefix"])
            .arg(&prefix)
            .args(["--base-url", &url])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &tools),
    )?;
    fs::write(root.join("installer.log"), output.stdout)?;
    download.abort();
    let binary = prefix.join("bin/gridstator");
    ensure!(
        metadata(&binary)?["buildId"] == base.manifest.build_id,
        "installed wrong actual executable"
    );
    let manual = root.join("manual prefix");
    fs::create_dir_all(manual.join("bin"))?;
    let target = base.manifest.target(release::host_target()?)?;
    let archive = base.directory.join(&target.archive.file);
    target.archive.verify(&archive)?;
    let bytes = command_ok(
        Command::new("tar")
            .args(["-xOzf"])
            .arg(&archive)
            .arg("gridstator"),
    )?
    .stdout;
    fs::write(manual.join("bin/gridstator"), bytes)?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            manual.join("bin/gridstator"),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    let manifest = base.directory.join(release::RELEASE_MANIFEST_NAME);
    command_ok(
        Command::new(manual.join("bin/gridstator"))
            .args(["release", "verify", "--manifest"])
            .arg(&manifest)
            .args(["--manifest-sha256", &base.manifest.sha256()?, "--archive"])
            .arg(&archive)
            .arg("--executable")
            .arg(manual.join("bin/gridstator"))
            .args(["--target", release::host_target()?])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &tools),
    )?;
    let manual_metadata = manual
        .join("share/gridstator/releases")
        .join(format!("v{}", base.manifest.version));
    fs::create_dir_all(&manual_metadata)?;
    fs::copy(
        &manifest,
        manual_metadata.join(release::RELEASE_MANIFEST_NAME),
    )?;
    fs::copy(
        base.directory.join("SHA256SUMS"),
        manual_metadata.join("SHA256SUMS"),
    )?;
    let directory = root.join("fresh-native");
    let login = root.join("client.json");
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let native_port = listener.local_addr()?.port().to_string();
    drop(listener);

    command_ok(
        Command::new(&binary)
            .args([
                "controller",
                "init",
                "--host",
                "127.0.0.1",
                "--port",
                &native_port,
                "--data-dir",
            ])
            .arg(&directory)
            .env("GRIDSTATOR_CONFIG", &login)
            .env("HOME", &home),
    )?;
    let fresh_config: Config = serde_json::from_slice(&fs::read(&login)?)?;
    let initial_id = state::read_json(&directory.join("controller.json"))?["clusterId"].clone();
    let store = InstallStore::new(root.join("managed"));
    store.install_verified(&base.manifest, release::host_target()?, &archive)?;
    store.activate(&base.manifest.build_id, None)?;
    store.install_supervisor(&binary, false)?;
    let mirror = ArtifactMirror::new(directory.join("native-release-mirror"));
    mirror.cache_manifest(&base.manifest.to_bytes()?, &base.manifest.sha256()?)?;
    for (key, artifact) in &base.manifest.artifacts {
        mirror.cache_artifact(
            &base.manifest,
            key,
            base.directory.join(&artifact.archive.file),
        )?;
    }
    {
        let database = gridstator::db::Db::open(&directory.join("controller.db"))?;
        database.write(|connection|gridstator::db::put(connection,"settings","cluster",&json!({"jobRetentionDays":90,"logRetentionDays":30,"snapshotRetentionDays":7,"autoUpdate":"off","updateWindow":null,"toolchain":{"act":"off"}}))).await?;
    }
    let mut server = native_role(&root, &store, &directory, &login, &home)?;
    let service_name = server.name();
    let client = client_for(&directory, &login, Some(&base.manifest.build_id)).await?;
    embedded_ready(&client, &base.manifest.build_id).await?;
    let join = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"fresh-rejoin","maxUses":1}),
        )
        .await?;
    command_ok(
        Command::new(&binary)
            .args([
                "agent",
                "join",
                &client.config.url,
                "--token",
                join["token"].as_str().unwrap(),
                "--fingerprint",
                &client.config.ca_fingerprint,
                "--name",
                "fresh-rejoined-worker",
                "--data-dir",
            ])
            .arg(root.join("native-agent"))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &tools),
    )?;
    ensure!(
        state::read_json(&root.join("native-agent/agent.json"))?["version"] == 2,
        "fresh worker credentials not persisted"
    );
    let marker=client.post("/v1/jobs",json!({"spec":{"kind":"command","command":["echo","restore-me"],"name":"pre-update history marker"}})).await?;
    let marker_id = marker["id"].as_str().unwrap();
    publish(&client, &good).await?;
    let client = client_for(&directory, &login, Some(&good.manifest.build_id)).await?;
    embedded_ready(&client, &good.manifest.build_id).await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if store.state()?.is_some_and(|state| {
                state.current == good.manifest.build_id && state.pending.is_none()
            }) {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    ensure!(
        client.get(&format!("/v1/jobs/{marker_id}")).await?["name"] == "pre-update history marker",
        "successful update lost history"
    );
    ensure!(
        !directory.join("updates/controller-switch.json").exists(),
        "confirmed update kept stale recovery marker"
    );
    let before = db_facts(&directory.join("controller.db"))?;
    publish(&client, &bad).await?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if store.state()?.is_some_and(|state| {
                state.current == good.manifest.build_id
                    && state.pending.is_none()
                    && state
                        .last_rollback
                        .is_some_and(|rollback| rollback.from == bad.manifest.build_id)
            }) {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let client = client_for(&directory, &login, Some(&good.manifest.build_id)).await?;
    embedded_ready(&client, &good.manifest.build_id).await?;
    ensure!(
        client.get(&format!("/v1/jobs/{marker_id}")).await?["name"] == "pre-update history marker",
        "rollback did not restore history"
    );
    let restored = db_facts(&directory.join("controller.db"))?;
    ensure!(
        restored == before,
        "schema/row restoration differs: {before} -> {restored}"
    );
    let failed = fs::read_dir(&directory)?
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("controller.db.failed-{}-", bad.manifest.build_id))
                && !entry.file_name().to_string_lossy().ends_with("-wal")
                && !entry.file_name().to_string_lossy().ends_with("-shm")
        })
        .context("failed candidate DB was not retained")?
        .path();
    let failed_facts = db_facts(&failed)?;
    ensure!(
        failed_facts["candidateColumn"] == true
            && failed_facts["candidateTable"] == true
            && failed_facts["userVersion"] == 999
            && failed_facts["jobs"] == 0,
        "candidate did not actually mutate schema/history: {failed_facts}"
    );
    ensure!(
        fs::read_to_string(directory.join("candidate-mutated-schema"))? == bad.manifest.build_id,
        "candidate executable never ran"
    );
    let active_cluster = client.get("/v1/cluster").await?;
    ensure!(
        active_cluster["clusterId"] == initial_id,
        "upgrade/rollback changed cluster identity"
    );
    ensure!(
        client.config.ca_pem == fresh_config.ca_pem,
        "upgrade changed native CA"
    );
    server.stop(&root)?;
    let rollback = store
        .state()?
        .unwrap()
        .last_rollback
        .context("rollback record")?;
    ensure!(
        rollback.reason == gridstator::install::RollbackReason::Crashed,
        "candidate did not roll back after crashes"
    );
    let report = json!({"scope":"actual Linux host installer/manual/controller update and schema rollback; other target entries are parsing fixtures only","target":release::host_target()?,"sourceRevision":gridstator::SOURCE_REVISION,"nativeUserService":service_name,"payloadSourceDirectory":payload_source,"prebuiltPayloads":prebuilt.is_some(),"baseline":base.manifest.build_id,"confirmed":good.manifest.build_id,"failed":bad.manifest.build_id,"before":before,"restored":restored,"failedDatabase":failed_facts,"rollback":store.state()?.unwrap().last_rollback,"clusterId":initial_id});
    fs::write(
        root.join("rehearsal.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!(
        "Recovery rehearsal passed: {}",
        root.join("rehearsal.json").display()
    );
    Ok(())
}
