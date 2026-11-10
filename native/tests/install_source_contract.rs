//! Exact shared install acceptance, replacing npm packages/Node with verified native payloads.
#![cfg(unix)]
use gridstator::{
    install::{InstallStore, SupervisorOptions, supervise},
    release::{
        FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest, SUPPORTED_TARGETS,
        host_target,
    },
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};

fn payload() -> PathBuf {
    static FIXTURE: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let temp=tempfile::tempdir().unwrap();let source=temp.path().join("payload.c");let binary=temp.path().join("payload");
        fs::write(&source,r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc,char **argv) {
 const char *id=getenv("GRIDSTATOR_INSTALL_VERSION");
 const char *path=getenv("TRACE");
 if(path) {FILE *f=fopen(path,"ab");if(!f)return 90;fprintf(f,"%s\n",id?id:"");fclose(f);}
 path=getenv("ARGV_TRACE");
 if(path) {FILE *f=fopen(path,"wb");if(!f)return 91;for(int i=0;i<argc;i++)fwrite(argv[i],1,strlen(argv[i])+1,f);fclose(f);}
 path=getenv("ENV_TRACE");
 if(path) {FILE *f=fopen(path,"wb");if(!f)return 92;const char *names[]={"GRIDSTATOR_INSTALL_ROOT","GRIDSTATOR_INSTALL_VERSION","GRIDSTATOR_BUILD_ID","GRIDSTATOR_INSTALL_TARGET","GRIDSTATOR_SUPERVISED"};for(int i=0;i<5;i++){const char *v=getenv(names[i]);if(v)fwrite(v,1,strlen(v),f);fputc(0,f);}fclose(f);}
 const char *fail=getenv("FAIL_BUILD");if(fail&&id&&!strcmp(fail,id))return 7;
 const char *hold=getenv("HOLD_BUILD");const char *release=getenv("RELEASE_FILE");
 if(hold&&id&&!strcmp(hold,id))for(int i=0;i<1000&&(!release||access(release,F_OK));i++)usleep(10000);
 return 0;
}
"#).unwrap();
        let mut compiler=std::process::Command::new("cc");
        #[cfg(target_os="macos")]
        compiler.args(["-arch",if cfg!(target_arch="aarch64"){"arm64"}else{"x86_64"}]);
        let output=compiler.arg(source).arg("-o").arg(&binary).output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
        (temp,binary)
    }).1.clone()
}
fn release(temp: &Path, id: &str) -> (ReleaseManifest, PathBuf) {
    let binary = payload();
    let bytes = fs::read(&binary).unwrap();
    let archive = temp.join(format!("{id}.tar.gz"));
    let encoder = flate2::write::GzEncoder::new(
        fs::File::create(&archive).unwrap(),
        flate2::Compression::fast(),
    );
    let mut tar = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o755);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    tar.append_data(&mut header, "gridstator", bytes.as_slice())
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    let artifact = ReleaseArtifact {
        archive: FileDigest::from_path(archive.file_name().unwrap().to_str().unwrap(), &archive)
            .unwrap(),
        executable: FileDigest::from_path("gridstator", &binary).unwrap(),
    };
    (
        ReleaseManifest {
            schema_version: 1,
            build_id: id.into(),
            version: "2.0.0".into(),
            source_revision: "controlled-native-C-test-payload".into(),
            protocol: ProtocolCompatibility::default(),
            artifacts: SUPPORTED_TARGETS
                .into_iter()
                .map(|target| (target.into(), artifact.clone()))
                .collect(),
        },
        archive,
    )
}
fn store(temp: &Path) -> InstallStore {
    let store = InstallStore::new(temp.join("install with spaces"));
    for id in ["old", "candidate"] {
        let (manifest, archive) = release(temp, id);
        store
            .install_verified(&manifest, host_target().unwrap(), archive)
            .unwrap();
    }
    store.activate("old", None).unwrap();
    store
}
fn options() -> SupervisorOptions {
    SupervisorOptions {
        poll_interval: Duration::from_millis(5),
        restart_delay: Duration::from_millis(5),
        shutdown_grace: Duration::from_millis(100),
        forward_signals: false,
        ..Default::default()
    }
}

#[test]
fn source_install_001_and_004_native_identity_is_embedded_and_digest_describes_current_bytes() {
    let binary = Path::new(env!("CARGO_BIN_EXE_gridstator"));
    let inspect = |path: &Path| {
        let output = std::process::Command::new(path)
            .args(["build-info", "--json"])
            .env("GRIDSTATOR_INSTALL_VERSION", "forged-runtime-build")
            .env("GRIDSTATOR_BUILD_ID", "forged-runtime-build")
            .env("GRIDSTATOR_SOURCE_REVISION", "forged-runtime-source")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let info = inspect(binary);
    assert_eq!(info["buildId"], gridstator::BUILD_ID);
    assert_eq!(info["version"], gridstator::VERSION);
    assert_eq!(info["sourceRevision"], gridstator::SOURCE_REVISION);
    assert_eq!(info["target"], host_target().unwrap());
    assert_eq!(
        info["sha256"],
        gridstator::release::hash_file(binary).unwrap().1
    );
    let temp = tempfile::tempdir().unwrap();
    let packaged = temp.path().join("packaged native executable with spaces");
    fs::copy(binary, &packaged).unwrap();
    assert_eq!(
        inspect(&packaged),
        info,
        "native identity is independent of workspace/package location"
    );
    let store = store(temp.path());
    let (mut original, _) = release(temp.path(), "old");
    let altered = temp.path().join("altered executable");
    let mut bytes = fs::read(payload()).unwrap();
    bytes.push(0);
    fs::write(&altered, bytes).unwrap();
    for artifact in original.artifacts.values_mut() {
        artifact.executable = FileDigest::from_path("gridstator", &altered).unwrap();
    }
    assert!(
        store
            .install_verified_executable(&original, host_target().unwrap(), &altered)
            .is_err(),
        "an allocated native ID cannot be rebound to different content"
    );
    assert_eq!(store.state().unwrap().unwrap().current, "old");
}

#[test]
fn source_install_002_and_005_native_executable_and_archive_install_identically_and_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let (manifest, archive) = release(temp.path(), "allocated-build");
    let direct = InstallStore::new(temp.path().join("direct"));
    let tar = InstallStore::new(temp.path().join("tar"));
    direct
        .install_verified_executable(&manifest, host_target().unwrap(), payload())
        .unwrap();
    tar.install_verified(&manifest, host_target().unwrap(), &archive)
        .unwrap();
    let before = fs::read(tar.executable(&manifest.build_id).unwrap()).unwrap();
    tar.install_verified(&manifest, host_target().unwrap(), &archive)
        .unwrap();
    let a = direct.verify_installed(&manifest.build_id).unwrap();
    let b = tar.verify_installed(&manifest.build_id).unwrap();
    assert_eq!(a.build_id, b.build_id);
    assert_eq!(a.target, b.target);
    assert_eq!(a.manifest_sha256, b.manifest_sha256);
    assert_eq!(fs::read(a.executable).unwrap(), before);
    assert_eq!(fs::read(b.executable).unwrap(), before);
    assert!(direct.state().unwrap().is_none());
    assert!(tar.state().unwrap().is_none());
}
#[tokio::test]
async fn source_install_006_forwards_exact_arguments_and_verified_launcher_environment() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let trace = temp.path().join("argv");
    let env = temp.path().join("environment");
    let original = [
        "agent",
        "start",
        "--data-dir",
        "/d with spaces",
        "",
        "quote\"line\n🦀",
        "--literal",
    ];
    let mut options = options();
    options.args = original.iter().map(Into::into).collect();
    options.environment = BTreeMap::from([
        ("ARGV_TRACE".into(), trace.as_os_str().into()),
        ("ENV_TRACE".into(), env.as_os_str().into()),
        ("GRIDSTATOR_INSTALL_VERSION".into(), "spoofed".into()),
        ("GRIDSTATOR_INSTALL_ROOT".into(), "/wrong".into()),
    ]);
    let outcome = supervise(&store, options).await.unwrap();
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.build_id, "old");
    let read = |path: &Path| {
        fs::read(path)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .map(|value| String::from_utf8(value.to_vec()).unwrap())
            .collect::<Vec<_>>()
    };
    let bytes = fs::read(trace).unwrap();
    let mut arguments = bytes
        .split(|byte| *byte == 0)
        .map(|value| String::from_utf8(value.to_vec()).unwrap())
        .collect::<Vec<_>>();
    arguments.pop();
    assert_eq!(
        arguments[0],
        store.executable("old").unwrap().to_string_lossy()
    );
    assert_eq!(&arguments[1..], original);
    assert_eq!(
        read(&env),
        [
            store.root().to_string_lossy().into_owned(),
            "old".into(),
            "old".into(),
            host_target().unwrap().into(),
            "1".into()
        ]
    );
}
#[tokio::test]
async fn source_install_010_missing_active_version_has_actionable_identity() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    fs::remove_dir_all(store.root().join("versions/old")).unwrap();
    let error = supervise(&store, options()).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        error.to_string().contains("version old is missing"),
        "{error}"
    );
    let output = launcher(&store).output().await.unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("version old is missing"));
}
#[test]
fn source_install_003_rejects_unverified_launcher_identity_child() {
    if std::env::var_os("GRIDSTATOR_INSTALL_IDENTITY_TEST_CHILD").is_none() {
        let temp = tempfile::tempdir().unwrap();
        let original = std::env::current_exe().unwrap();
        let target = host_target().unwrap();
        let artifact = ReleaseArtifact {
            archive: FileDigest::from_path("gridstator", &original).unwrap(),
            executable: FileDigest::from_path("gridstator", &original).unwrap(),
        };
        let mut manifest = ReleaseManifest {
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
        let store = InstallStore::new(temp.path().join("verified store"));
        store
            .install_verified_executable(&manifest, target, &original)
            .unwrap();
        store.activate(gridstator::BUILD_ID, None).unwrap();
        let installed = store.executable(gridstator::BUILD_ID).unwrap();
        let run = |binary: &Path, root: &Path, version: &str, marker: &str, expected: bool| {
            let output = std::process::Command::new(binary)
                .args([
                    "--exact",
                    "source_install_003_rejects_unverified_launcher_identity_child",
                    "--nocapture",
                ])
                .env("GRIDSTATOR_INSTALL_IDENTITY_TEST_CHILD", "1")
                .env("GRIDSTATOR_IDENTITY_EXPECT_MANAGED", expected.to_string())
                .env("GRIDSTATOR_SUPERVISED", marker)
                .env("GRIDSTATOR_INSTALL_ROOT", root)
                .env("GRIDSTATOR_INSTALL_VERSION", version)
                .env("GRIDSTATOR_ACT", "/nonexistent-act-for-this-test")
                .env("PATH", temp.path().join("no-probe-tools"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "version={version}, marker={marker}, expected={expected}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&original, temp.path(), "forged-build-id", "1", false);
        run(&original, store.root(), gridstator::BUILD_ID, "1", false);
        run(&installed, store.root(), gridstator::BUILD_ID, "yes", false);
        run(&installed, store.root(), "forged-build-id", "1", false);
        run(&installed, store.root(), gridstator::BUILD_ID, "1", true);
        // The old real payload remains managed between provisional activation and supervisor
        // handoff. This protects its ability to finish a health/update reconciliation.
        manifest.build_id = "provisional-candidate".into();
        store
            .install_verified_executable(&manifest, target, &original)
            .unwrap();
        store
            .activate(&manifest.build_id, Some(Duration::from_secs(30)))
            .unwrap();
        run(&installed, store.root(), gridstator::BUILD_ID, "1", true);
        return;
    }
    let info = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(gridstator::worker::node_info())
        .unwrap();
    assert_eq!(
        info["build"]["id"],
        gridstator::BUILD_ID,
        "embedded identity cannot be replaced by unverified launcher environment"
    );
    assert_eq!(
        info["build"]["managed"],
        std::env::var("GRIDSTATOR_IDENTITY_EXPECT_MANAGED").as_deref() == Ok("true"),
        "missing/mismatched install metadata cannot claim native management"
    );
    let view =
        gridstator::updates::controller_build_view(Path::new("/no-controller-mirror-needed"));
    let current = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
        .and_then(|root| InstallStore::new(root).state().ok().flatten())
        .is_some_and(|state| state.current == gridstator::BUILD_ID);
    assert_eq!(
        view["selfUpdate"]["available"],
        info["build"]["managed"] == true && current,
        "controller restart capability must verify physical running identity as well"
    );
    if info["build"]["managed"] == true && !current {
        let store = InstallStore::new(std::env::var_os("GRIDSTATOR_INSTALL_ROOT").unwrap());
        let before = store.state().unwrap().unwrap();
        let confirmation = store.confirm_running_payload().unwrap();
        assert!(!confirmation.confirmed);
        assert!(!confirmation.supervisor_refreshed);
        assert!(confirmation.refresh_error.is_none());
        assert_eq!(
            store.state().unwrap().unwrap(),
            before,
            "old provisional payload cannot confirm the candidate or refresh its supervisor"
        );
    }
}

fn launcher(store: &InstallStore) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .arg("supervise")
        .arg("--install-root")
        .arg(store.root())
        .args(["--", "agent", "start", "--data-dir", "/d with spaces"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}
#[tokio::test]
async fn source_install_007_crash_rollback_preserves_state_and_explains_attempt_limit() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let trace = temp.path().join("trace");
    store
        .activate("candidate", Some(Duration::from_secs(60)))
        .unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        launcher(&store)
            .env("FAIL_BUILD", "candidate")
            .env("TRACE", &trace)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(trace).unwrap(),
        "candidate\ncandidate\ncandidate\nold\n"
    );
    let state = store.state().unwrap().unwrap();
    let rollback = state.last_rollback.unwrap();
    assert_eq!(state.current, "old");
    assert!(state.pending.is_none());
    assert!(state.previous.is_none());
    assert_eq!(rollback.from, "candidate");
    assert_eq!(rollback.to, "old");
    assert_eq!(
        rollback.reason,
        gridstator::install::RollbackReason::Crashed
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("failed to start 3 times; rolled back to old"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[tokio::test]
async fn source_install_008_expired_started_trial_rolls_back_with_clear_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let trace = temp.path().join("trace");
    store
        .activate("candidate", Some(Duration::from_secs(60)))
        .unwrap();
    let mut command = launcher(&store);
    command.env("HOLD_BUILD", "candidate").env("TRACE", &trace);
    let child = command.spawn().unwrap();
    let wait = tokio::spawn(async move { child.wait_with_output().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !trace.is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.root().join("install.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let mut state: gridstator::install::InstallState =
        serde_json::from_slice(&fs::read(store.root().join("state.json")).unwrap()).unwrap();
    state.pending.as_mut().unwrap().deadline = state.pending.as_ref().unwrap().first_launch_at;
    gridstator::state::write_json(
        &store.root().join("state.json"),
        &serde_json::to_value(&state).unwrap(),
    )
    .unwrap();
    drop(lock);
    let output = tokio::time::timeout(Duration::from_secs(15), wait)
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    let state = store.state().unwrap().unwrap();
    assert_eq!(state.current, "old");
    assert!(state.pending.is_none());
    assert!(state.previous.is_none());
    assert_eq!(
        state.last_rollback.unwrap().reason,
        gridstator::install::RollbackReason::TimedOut
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("was not confirmed in time"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[tokio::test]
async fn source_install_009_and_011_confirmed_native_version_keeps_arguments_and_single_trial_attempt()
 {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let trace = temp.path().join("trace");
    let release = temp.path().join("release");
    store
        .activate("candidate", Some(Duration::from_secs(30)))
        .unwrap();
    let mut trial_options = options();
    trial_options.args = vec!["agent".into(), "start".into()];
    trial_options.environment = BTreeMap::from([
        ("TRACE".into(), trace.as_os_str().into()),
        ("HOLD_BUILD".into(), "candidate".into()),
        ("RELEASE_FILE".into(), release.as_os_str().into()),
    ]);
    let (outcome, ()) = tokio::join!(supervise(&store, trial_options), async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !trace.is_file() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            store
                .state()
                .unwrap()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .attempts,
            1
        );
        assert!(!store.confirm("old").unwrap());
        assert!(store.confirm("candidate").unwrap());
        fs::write(&release, b"ready").unwrap();
    });
    assert_eq!(outcome.unwrap().build_id, "candidate");
    for _ in 0..4 {
        assert_eq!(
            supervise(&store, options()).await.unwrap().build_id,
            "candidate"
        );
    }
    let state = store.state().unwrap().unwrap();
    assert_eq!(state.current, "candidate");
    assert_eq!(state.previous.as_deref(), Some("old"));
    assert!(state.pending.is_none());
    assert!(state.last_rollback.is_none());
}
#[tokio::test]
async fn source_install_013_refreshes_older_native_launcher_then_runs_active_version() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let old = store.install_supervisor(payload(), false).unwrap();
    let old_digest = gridstator::release::hash_file(&old).unwrap().1;
    let current = env!("CARGO_BIN_EXE_gridstator");
    let expected = gridstator::release::hash_file(current).unwrap().1;
    assert_ne!(old_digest, expected);
    assert_eq!(store.install_supervisor(current, true).unwrap(), old);
    assert_eq!(gridstator::release::hash_file(&old).unwrap().1, expected);
    assert_eq!(store.install_supervisor(payload(), false).unwrap(), old);
    assert_eq!(
        gridstator::release::hash_file(&old).unwrap().1,
        expected,
        "an ordinary no-replace adoption preserves the refreshed launcher"
    );
    let output = tokio::process::Command::new(&old)
        .arg("supervise")
        .arg("--install-root")
        .arg(store.root())
        .args(["--", "agent", "start"])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn source_install_013_healthy_confirmation_refreshes_verified_supervisor_child() {
    if let Ok(mode) = std::env::var("GRIDSTATOR_HEALTH_CONFIRM_TEST_CHILD") {
        let store = InstallStore::new(std::env::var_os("GRIDSTATOR_INSTALL_ROOT").unwrap());
        let expected = gridstator::install::running_executable_digest().unwrap().1;
        if mode == "healthy" {
            assert_ne!(
                gridstator::release::hash_file(store.supervisor_path())
                    .unwrap()
                    .1,
                expected,
                "old stable launcher must remain until health confirmation"
            );
        } else {
            assert!(store.supervisor_path().is_dir());
        }
        let confirmation = store.confirm_running_payload().unwrap();
        assert!(confirmation.confirmed);
        assert!(store.state().unwrap().unwrap().pending.is_none());
        if mode == "healthy" {
            assert!(confirmation.supervisor_refreshed);
            assert!(confirmation.refresh_error.is_none());
            assert_eq!(
                gridstator::release::hash_file(store.supervisor_path())
                    .unwrap()
                    .1,
                expected,
                "healthy confirmation automatically refreshes the verified stable launcher"
            );
            let again = store.confirm_running_payload().unwrap();
            assert!(!again.confirmed);
            assert!(!again.supervisor_refreshed);
            assert!(again.refresh_error.is_none());
            // A cached executable hash must not hide changed expected metadata.
            let manifest_path = store
                .root()
                .join("versions")
                .join(gridstator::BUILD_ID)
                .join("release.json");
            let installed_path = manifest_path.parent().unwrap().join("installed.json");
            let manifest_bytes = fs::read(&manifest_path).unwrap();
            let installed_bytes = fs::read(&installed_path).unwrap();
            let mut changed = store.manifest(gridstator::BUILD_ID).unwrap();
            changed
                .artifacts
                .get_mut(host_target().unwrap())
                .unwrap()
                .executable
                .sha256 = "0".repeat(64);
            let mut installed: gridstator::install::InstalledRelease =
                serde_json::from_slice(&installed_bytes).unwrap();
            installed.manifest_sha256 = changed.sha256().unwrap();
            gridstator::state::write_json(&manifest_path, &serde_json::to_value(&changed).unwrap())
                .unwrap();
            gridstator::state::write_json(
                &installed_path,
                &serde_json::to_value(&installed).unwrap(),
            )
            .unwrap();
            assert!(
                store.verify_running_payload().is_err(),
                "cached bytes cannot bypass a changed manifest digest"
            );
            gridstator::state::write_atomic(&manifest_path, &manifest_bytes).unwrap();
            gridstator::state::write_atomic(&installed_path, &installed_bytes).unwrap();
            assert!(store.verify_running_payload().is_ok());
        } else {
            assert!(!confirmation.supervisor_refreshed);
            assert!(
                confirmation
                    .refresh_error
                    .unwrap()
                    .contains("remains confirmed")
            );
            assert!(
                store.supervisor_path().is_dir(),
                "failed refresh preserves the old destination"
            );
        }
        return;
    }
    for mode in ["healthy", "copy-fails"] {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let binary = std::env::current_exe().unwrap();
        let artifact = ReleaseArtifact {
            archive: FileDigest::from_path("gridstator", &binary).unwrap(),
            executable: FileDigest::from_path("gridstator", &binary).unwrap(),
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
        store
            .install_verified_executable(&manifest, host_target().unwrap(), &binary)
            .unwrap();
        if mode == "healthy" {
            store.install_supervisor(payload(), false).unwrap();
        } else {
            fs::create_dir(store.supervisor_path()).unwrap();
        }
        store
            .activate(gridstator::BUILD_ID, Some(Duration::from_secs(30)))
            .unwrap();
        let before = store.state().unwrap().unwrap();
        assert!(
            store
                .confirm_running_payload()
                .unwrap_err()
                .to_string()
                .contains("not the verified installed native payload"),
            "an uninstalled parent cannot confirm merely by naming the store"
        );
        assert_eq!(store.state().unwrap().unwrap(), before);
        let mut options = options();
        options.args = vec![
            "--exact".into(),
            "source_install_013_healthy_confirmation_refreshes_verified_supervisor_child".into(),
            "--nocapture".into(),
        ];
        options
            .environment
            .insert("GRIDSTATOR_HEALTH_CONFIRM_TEST_CHILD".into(), mode.into());
        let outcome = supervise(&store, options).await.unwrap();
        assert_eq!(
            outcome.code, 0,
            "installed payload must confirm without making refresh failure fatal"
        );
        let state = store.state().unwrap().unwrap();
        assert_eq!(state.current, gridstator::BUILD_ID);
        assert!(state.pending.is_none());
        assert!(state.last_rollback.is_none());
    }
}
