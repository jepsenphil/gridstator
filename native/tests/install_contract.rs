#![cfg(unix)]

use flate2::{Compression, write::GzEncoder};
use gridstator::{
    install::{InstallState, InstallStore, RollbackReason, SupervisorOptions, supervise},
    release::{
        ArtifactMirror, FileDigest, MANIFEST_VERSION, OfficialReleases, ProtocolCompatibility,
        RELEASE_MANIFEST_NAME, ReleaseArtifact, ReleaseManifest, SUPPORTED_TARGETS, checksum_for,
        host_target, sha256_bytes, target_for,
    },
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tempfile::TempDir;

fn native_fixture() -> PathBuf {
    static FIXTURE: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            // macOS system shells may be universal Mach-O. Compile a thin host executable so the
            // production architecture gate stays strict. Its argv preserves controlled scripts.
            let temp = TempDir::new().unwrap();
            let source = temp.path().join("shell-launcher.c");
            let binary = temp.path().join("shell-launcher");
            fs::write(
                &source,
                b"#include <unistd.h>\nint main(int argc, char **argv) { (void)argc; argv[0] = \"/bin/sh\"; execv(\"/bin/sh\", argv); return 127; }\n",
            )
            .unwrap();
            let mut compiler = std::process::Command::new("cc");
            #[cfg(target_os = "macos")]
            compiler.args([
                "-arch",
                if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" },
            ]);
            let output = compiler.arg(&source).arg("-o").arg(&binary).output().expect(
                "native installer contract tests require a host C compiler (cc)",
            );
            assert!(
                output.status.success(),
                "could not compile native shell launcher: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            gridstator::release::verify_executable_target(&binary, host_target().unwrap()).unwrap();
            (temp, binary)
        })
        .1
        .clone()
}

fn manifest(
    build_id: &str,
    archive: &Path,
    archive_name: &str,
    executable: &Path,
    executable_name: &str,
) -> ReleaseManifest {
    let artifact = ReleaseArtifact {
        archive: FileDigest::from_path(archive_name, archive).unwrap(),
        executable: FileDigest::from_path(executable_name, executable).unwrap(),
    };
    ReleaseManifest {
        schema_version: MANIFEST_VERSION,
        build_id: build_id.into(),
        version: gridstator::VERSION.into(),
        source_revision: "test-revision".into(),
        protocol: ProtocolCompatibility::default(),
        artifacts: SUPPORTED_TARGETS
            .into_iter()
            .map(|target| (target.into(), artifact.clone()))
            .collect(),
    }
}

fn install_fixture(store: &InstallStore, build_id: &str) -> ReleaseManifest {
    let shell = native_fixture();
    let value = manifest(build_id, &shell, "gridstator-native", &shell, "gridstator");
    store
        .install_verified(&value, host_target().unwrap(), &shell)
        .unwrap();
    value
}

fn prepare_store(temp: &TempDir) -> InstallStore {
    let store = InstallStore::new(temp.path().join("store with spaces"));
    install_fixture(&store, "old");
    store.activate("old", None).unwrap();
    install_fixture(&store, "candidate");
    store
}

fn script(temp: &TempDir, body: &str) -> PathBuf {
    let path = temp.path().join("controlled child.sh");
    fs::write(&path, body).unwrap();
    path
}

fn options(script: &Path, trace: &Path) -> SupervisorOptions {
    SupervisorOptions {
        args: vec![script.as_os_str().into()],
        environment: BTreeMap::from([(OsString::from("TRACE"), trace.as_os_str().into())]),
        poll_interval: Duration::from_millis(5),
        restart_delay: Duration::from_millis(5),
        shutdown_grace: Duration::from_millis(30),
        forward_signals: false,
        ..Default::default()
    }
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("controlled child started");
}

#[test]
fn release_identity_is_independent_of_digest_and_protocol_names_are_explicit() {
    let shell = native_fixture();
    let value = manifest(
        "allocated-before-compilation",
        &shell,
        "gridstator",
        &shell,
        "gridstator",
    );
    let bytes = value.to_bytes().unwrap();
    assert_eq!(ReleaseManifest::from_bytes(&bytes).unwrap(), value);
    assert_eq!(value.sha256().unwrap(), sha256_bytes(&bytes));
    assert_ne!(value.build_id, value.sha256().unwrap());
    assert_eq!(target_for("linux", "x64"), Some("linux/amd64"));
    assert_eq!(target_for("macos", "aarch64"), Some("darwin/arm64"));
    assert_eq!(target_for("windows", "x86_64"), None);
    let mut incomplete = value.clone();
    incomplete.artifacts.clear();
    assert!(incomplete.validate().is_err());
    let mut unsupported = value.clone();
    let artifact = unsupported.artifacts["linux/amd64"].clone();
    unsupported
        .artifacts
        .insert("windows/amd64".into(), artifact);
    assert!(unsupported.validate().is_err());
    let mut traversal = value.clone();
    traversal
        .artifacts
        .get_mut("linux/amd64")
        .unwrap()
        .executable
        .file = "../gridstator".into();
    assert!(traversal.validate().is_err());
    let duplicate = String::from_utf8(bytes).unwrap().replacen(
        "\"artifacts\": {",
        &format!(
            "\"artifacts\": {{\"linux/amd64\": {},",
            serde_json::to_string(value.target("linux/amd64").unwrap()).unwrap()
        ),
        1,
    );
    assert!(ReleaseManifest::from_bytes(duplicate.as_bytes()).is_err());
}

#[test]
fn checksum_selection_rejects_missing_and_ambiguous_entries() {
    let digest = "a".repeat(64);
    assert_eq!(
        checksum_for(&format!("{digest}  release.json\n"), "release.json").unwrap(),
        digest
    );
    assert!(
        checksum_for(
            &format!("{digest}  release.json\n{digest} *release.json\n"),
            "release.json"
        )
        .is_err()
    );
    assert!(checksum_for("bad  release.json\n", "release.json").is_err());
    assert!(checksum_for(&format!("{digest}  other\n"), "release.json").is_err());
}

#[test]
fn verified_install_is_staged_private_immutable_and_does_not_activate() {
    let temp = TempDir::new().unwrap();
    let store = InstallStore::new(temp.path().join("store"));
    let value = install_fixture(&store, "build-one");
    assert!(store.state().unwrap().is_none());
    let installed = store.verify_installed("build-one").unwrap();
    assert_eq!(
        fs::metadata(&installed.executable)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(store.root()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let original = value.sha256().unwrap();
    let mut conflicting = value.clone();
    conflicting.source_revision = "different-build".into();
    assert!(
        store
            .install_verified(&conflicting, host_target().unwrap(), native_fixture())
            .is_err()
    );
    assert_eq!(
        store.manifest("build-one").unwrap().sha256().unwrap(),
        original
    );
    let state = store.activate("build-one", None).unwrap();
    assert_eq!(state.current, "build-one");
    assert!(state.pending.is_none());
    assert_eq!(
        fs::metadata(store.root().join("state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let bootstrap = InstallStore::new(temp.path().join("bootstrap store"));
    bootstrap
        .install_verified_executable(&value, host_target().unwrap(), native_fixture())
        .unwrap();
    assert_eq!(
        bootstrap.verify_installed("build-one").unwrap().build_id,
        "build-one"
    );
}

#[test]
fn corrupt_and_wrong_target_artifacts_never_become_visible() {
    let temp = TempDir::new().unwrap();
    let store = InstallStore::new(temp.path().join("store"));
    let shell = native_fixture();
    let mut value = manifest("bad", &shell, "gridstator", &shell, "gridstator");
    value
        .artifacts
        .get_mut(host_target().unwrap())
        .unwrap()
        .archive
        .sha256 = "0".repeat(64);
    assert!(
        store
            .install_verified(&value, host_target().unwrap(), &shell)
            .is_err()
    );
    assert!(!store.executable("bad").unwrap().exists());
    let other = if host_target().unwrap() == "linux/amd64" {
        "linux/arm64"
    } else {
        "linux/amd64"
    };
    assert!(store.install_verified(&value, other, &shell).is_err());
    let script = temp.path().join("looks executable");
    fs::write(
        &script,
        format!("#!/bin/sh\n# {}\nexit 0\n", "padding".repeat(20)),
    )
    .unwrap();
    let value = manifest("script", &script, "gridstator", &script, "gridstator");
    assert!(
        store
            .install_verified(&value, host_target().unwrap(), &script)
            .is_err()
    );
    assert!(!store.executable("script").unwrap().exists());
}

fn tar_archive(path: &Path, entries: &[(&str, &[u8], tar::EntryType)]) {
    let gzip = GzEncoder::new(fs::File::create(path).unwrap(), Compression::fast());
    let mut builder = tar::Builder::new(gzip);
    for (name, bytes, kind) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(bytes.len() as u64);
        header.set_entry_type(*kind);
        // Raw path construction deliberately permits malicious fixture names rejected by set_path.
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        if kind.is_symlink() {
            header.set_link_name("/outside").unwrap();
        }
        header.set_cksum();
        builder.append(&header, *bytes).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

#[test]
fn archive_extraction_verifies_inner_executable_and_rejects_links_traversal_duplicates() {
    let temp = TempDir::new().unwrap();
    let store = InstallStore::new(temp.path().join("store"));
    let shell = native_fixture();
    let bytes = fs::read(&shell).unwrap();
    let archive = temp.path().join("good.tar.gz");
    tar_archive(
        &archive,
        &[
            ("package/gridstator", &bytes, tar::EntryType::Regular),
            ("LICENSE", b"license", tar::EntryType::Regular),
        ],
    );
    let value = manifest(
        "archive",
        &archive,
        "gridstator.tar.gz",
        &shell,
        "package/gridstator",
    );
    store
        .install_verified(&value, host_target().unwrap(), &archive)
        .unwrap();
    assert_eq!(
        fs::read(store.executable("archive").unwrap()).unwrap(),
        bytes
    );
    for (index, entry) in [
        ("../escape", b"x".as_slice(), tar::EntryType::Regular),
        ("link", b"".as_slice(), tar::EntryType::Symlink),
        (
            "package/gridstator",
            bytes.as_slice(),
            tar::EntryType::Regular,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let archive = temp.path().join(format!("bad-{index}.tgz"));
        tar_archive(
            &archive,
            &[
                ("package/gridstator", &bytes, tar::EntryType::Regular),
                entry,
            ],
        );
        let id = format!("bad-archive-{index}");
        let value = manifest(
            &id,
            &archive,
            "gridstator.tgz",
            &shell,
            "package/gridstator",
        );
        assert!(
            store
                .install_verified(&value, host_target().unwrap(), &archive)
                .is_err()
        );
        assert!(!store.executable(&id).unwrap().exists());
    }
    assert!(!temp.path().join("escape").exists());
    let mut corrupt = value.clone();
    corrupt.build_id = "wrong-inner-digest".into();
    corrupt
        .artifacts
        .get_mut(host_target().unwrap())
        .unwrap()
        .executable
        .sha256 = "0".repeat(64);
    assert!(
        store
            .install_verified(&corrupt, host_target().unwrap(), &archive)
            .is_err()
    );
}

#[test]
fn simultaneous_installs_are_idempotent_and_cannot_overwrite_pending_activation() {
    let temp = TempDir::new().unwrap();
    let store = InstallStore::new(temp.path().join("store"));
    let threads = (0..4)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || install_fixture(&store, "one"))
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    store.activate("one", None).unwrap();
    install_fixture(&store, "two");
    install_fixture(&store, "three");
    store
        .activate("two", Some(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(
        store
            .activate("three", Some(Duration::from_secs(10)))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(store.state().unwrap().unwrap().current, "two");
    assert!(!store.confirm("one").unwrap());
    assert!(
        store.confirm("two").is_err(),
        "unlaunched payload cannot confirm"
    );
}

#[tokio::test]
async fn supervisor_retries_crashes_three_times_then_launches_previous_payload() {
    let temp = TempDir::new().unwrap();
    let store = prepare_store(&temp);
    let trace = temp.path().join("trace");
    let script = script(
        &temp,
        "printf '%s\\n' \"$GRIDSTATOR_INSTALL_VERSION\" >> \"$TRACE\"\nif [ \"$GRIDSTATOR_INSTALL_VERSION\" = candidate ]; then exit 23; fi\nexit 0\n",
    );
    store
        .activate("candidate", Some(Duration::from_secs(5)))
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        supervise(&store, options(&script, &trace)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.build_id, "old");
    assert_eq!(
        outcome.last_rollback.unwrap().reason,
        RollbackReason::Crashed
    );
    assert_eq!(
        fs::read_to_string(trace).unwrap(),
        "candidate\ncandidate\ncandidate\nold\n"
    );
    assert!(store.state().unwrap().unwrap().pending.is_none());
}

#[tokio::test]
async fn supervisor_kills_hung_candidate_and_rolls_back_on_deadline() {
    let temp = TempDir::new().unwrap();
    let store = prepare_store(&temp);
    let trace = temp.path().join("trace");
    // Deliberately exceed the former 80ms trial window before publishing readiness.
    let script = script(
        &temp,
        "sleep 0.4\nif [ \"$GRIDSTATOR_INSTALL_VERSION\" = candidate ]; then\n  trap '' TERM\n  echo $$ > \"$TRACE\"\n  exec sleep 30\nfi\nexit 0\n",
    );
    store
        .activate("candidate", Some(Duration::from_secs(30)))
        .unwrap();
    let opts = options(&script, &trace);
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(supervise(&store, opts), async {
            wait_for_file(&trace).await;
            expire_started_trial(&store);
        })
    })
    .await
    .unwrap();
    let outcome = outcome.unwrap();
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.build_id, "old");
    assert_eq!(
        outcome.last_rollback.unwrap().reason,
        RollbackReason::TimedOut
    );
    let pid: i32 = fs::read_to_string(trace).unwrap().trim().parse().unwrap();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "timed-out direct child was reaped"
    );
}

fn expire_started_trial(store: &InstallStore) {
    // Expire the real persisted trial only after its payload is ready. This exercises the
    // supervisor's deadline polling and escalation without racing a short wall-clock window
    // against process startup, fsync, or unrelated tests. Use the same lock as production writes.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.root().join("install.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let state_path = store.root().join("state.json");
    let mut state: InstallState = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let pending = state.pending.as_mut().expect("started candidate trial");
    pending.deadline = Some(
        pending
            .first_launch_at
            .expect("trial starts at payload launch"),
    );
    gridstator::state::write_json(&state_path, &serde_json::to_value(state).unwrap()).unwrap();
}

fn process_is_live(pid: u32) -> bool {
    let mut system = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    system
        .process(pid)
        .is_some_and(|process| process.status() != sysinfo::ProcessStatus::Zombie)
}

// A real child executable lets the regression create a different process group without requiring
// Python, Node, Docker, or a platform-specific setsid command. The ordinary test invocation is inert.
#[test]
fn supervisor_session_process_helper() {
    let Ok(mode) = std::env::var("GRIDSTATOR_SESSION_HELPER_MODE") else {
        return;
    };
    let trace = PathBuf::from(std::env::var_os("GRIDSTATOR_SESSION_HELPER_TRACE").unwrap());
    if mode == "check" {
        let record = fs::read_to_string(&trace).unwrap();
        let ids: Vec<u32> = record
            .split_whitespace()
            .map(|value| value.parse().unwrap())
            .collect();
        assert!(
            !process_is_live(ids[0]),
            "old executor group survived replacement"
        );
        let unrelated: u32 = std::env::var("GRIDSTATOR_UNRELATED_PID")
            .unwrap()
            .parse()
            .unwrap();
        assert!(process_is_live(unrelated), "unrelated process was killed");
        fs::write(
            trace.with_extension("checked"),
            "clean before replacement\n",
        )
        .unwrap();
        return;
    }
    assert_eq!(mode, "spawn");
    // SAFETY: this controlled helper changes only its own group and queries its own identifiers.
    assert_eq!(unsafe { libc::setpgid(0, 0) }, 0);
    fs::write(
        trace,
        format!(
            "{} {} {}\n",
            std::process::id(),
            unsafe { libc::getsid(0) },
            unsafe { libc::getpgrp() }
        ),
    )
    .unwrap();
    loop {
        // SAFETY: wait for the supervisor to clean up this owned helper process.
        unsafe { libc::pause() };
    }
}

fn session_options(script: &Path, trace: &Path, helper: &Path) -> SupervisorOptions {
    let mut opts = options(script, trace);
    opts.max_attempts = 1;
    opts.environment.extend([
        (
            OsString::from("GRIDSTATOR_SESSION_HELPER_MODE"),
            OsString::from("spawn"),
        ),
        (
            OsString::from("GRIDSTATOR_SESSION_HELPER_TRACE"),
            helper.as_os_str().into(),
        ),
        (
            OsString::from("GRIDSTATOR_SESSION_HELPER_BIN"),
            std::env::current_exe().unwrap().into_os_string(),
        ),
    ]);
    opts
}

struct SessionHelperCleanup(PathBuf);

impl Drop for SessionHelperCleanup {
    fn drop(&mut self) {
        let Ok(record) = fs::read_to_string(&self.0) else {
            return;
        };
        let ids: Vec<libc::pid_t> = record
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .collect();
        if let [pid, session, group] = ids.as_slice()
            && *pid > 1
            && pid == group
            && unsafe { libc::getsid(*pid) } == *session
            && unsafe { libc::getpgid(*pid) } == *group
        {
            // SAFETY: the fixture created this exact helper group; remove it even if an assertion
            // catches a regression before the supervisor has performed the expected cleanup.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    }
}

#[tokio::test]
async fn supervisor_reaps_separate_executor_groups_before_replacement() {
    // Exercise both normal exit and a hard crash: neither runs a payload's Rust drop handlers.
    for finish in ["exit 23", "kill -KILL $$"] {
        let temp = TempDir::new().unwrap();
        let store = prepare_store(&temp);
        let trace = temp.path().join("trace");
        let helper = temp.path().join("executor pid");
        let _helper_cleanup = SessionHelperCleanup(helper.clone());
        let script = script(
            &temp,
            &format!(
                "if [ \"$GRIDSTATOR_INSTALL_VERSION\" = candidate ]; then\n\
                 \"$GRIDSTATOR_SESSION_HELPER_BIN\" --exact supervisor_session_process_helper --nocapture &\n\
                 while [ ! -s \"$GRIDSTATOR_SESSION_HELPER_TRACE\" ]; do sleep 0.01; done\n\
                 echo $$ > \"$TRACE\"\n{finish}\nfi\n\
                 GRIDSTATOR_SESSION_HELPER_MODE=check \"$GRIDSTATOR_SESSION_HELPER_BIN\" --exact supervisor_session_process_helper --nocapture\n"
            ),
        );
        let mut unrelated = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut opts = session_options(&script, &trace, &helper);
        opts.environment.insert(
            OsString::from("GRIDSTATOR_UNRELATED_PID"),
            unrelated.id().unwrap().to_string().into(),
        );
        store
            .activate("candidate", Some(Duration::from_secs(5)))
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(10), supervise(&store, opts))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.build_id, "old");
        assert_eq!(outcome.code, 0);
        let ids: Vec<u32> = fs::read_to_string(&helper)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse().unwrap())
            .collect();
        let payload: u32 = fs::read_to_string(trace).unwrap().trim().parse().unwrap();
        assert_eq!(
            ids[1], payload,
            "executor inherits its payload's unique session"
        );
        assert_eq!(ids[0], ids[2], "executor owns a distinct process group");
        assert_ne!(ids[2], payload);
        assert_eq!(
            fs::read_to_string(helper.with_extension("checked")).unwrap(),
            "clean before replacement\n"
        );
        unrelated.kill().await.unwrap();
        unrelated.wait().await.unwrap();
    }
}

#[tokio::test]
async fn dropping_supervisor_stops_its_separate_executor_groups() {
    let temp = TempDir::new().unwrap();
    let store = prepare_store(&temp);
    let trace = temp.path().join("trace");
    let helper = temp.path().join("executor pid");
    let _helper_cleanup = SessionHelperCleanup(helper.clone());
    let script = script(
        &temp,
        "\"$GRIDSTATOR_SESSION_HELPER_BIN\" --exact supervisor_session_process_helper --nocapture &\n\
         while [ ! -s \"$GRIDSTATOR_SESSION_HELPER_TRACE\" ]; do sleep 0.01; done\n\
         echo $$ > \"$TRACE\"\nwait\n",
    );
    let opts = session_options(&script, &trace, &helper);
    let task = tokio::spawn(async move { supervise(&store, opts).await });
    wait_for_file(&trace).await;
    let pid: u32 = fs::read_to_string(&helper)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(process_is_live(pid));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        while process_is_live(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping supervisor kills its owned executor groups");
}

#[tokio::test]
async fn launch_based_deadline_allows_prelaunch_wait_and_healthy_confirmation() {
    let temp = TempDir::new().unwrap();
    let store = prepare_store(&temp);
    let trace = temp.path().join("trace");
    // Keep the healthy payload alive until confirmation instead of racing a 300ms sleep.
    let script = script(
        &temp,
        "sleep 0.4\necho started > \"$TRACE\"\nwhile [ ! -f \"$TRACE.confirmed\" ]; do sleep 0.02; done\nexit 0\n",
    );
    store
        .activate("candidate", Some(Duration::from_secs(30)))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    let pending = store.state().unwrap().unwrap().pending.unwrap();
    assert_eq!(pending.first_launch_at, None);
    assert_eq!(pending.deadline, None);
    let opts = options(&script, &trace);
    // Join the supervision future directly: an assertion failure drops its owned session before
    // the fixture directory is removed, instead of leaving a detached launcher behind.
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(supervise(&store, opts), async {
            wait_for_file(&trace).await;
            let pending = store.state().unwrap().unwrap().pending.unwrap();
            assert_eq!(
                pending.deadline.unwrap() - pending.first_launch_at.unwrap(),
                30_000
            );
            let second = supervise(&store, options(&script, &trace))
                .await
                .unwrap_err();
            assert_eq!(second.kind(), io::ErrorKind::WouldBlock);
            assert!(store.confirm("candidate").unwrap());
            fs::write(trace.with_extension("confirmed"), b"confirmed").unwrap();
        })
    })
    .await
    .unwrap();
    let outcome = outcome.unwrap();
    assert_eq!(outcome.build_id, "candidate");
    assert_eq!(outcome.code, 0);
    assert!(outcome.last_rollback.is_none());
    assert!(store.state().unwrap().unwrap().pending.is_none());
}

#[test]
fn interrupted_staging_does_not_change_the_active_payload() {
    let temp = TempDir::new().unwrap();
    let store = prepare_store(&temp);
    fs::create_dir(store.root().join("versions/.staging-interrupted")).unwrap();
    fs::write(
        store
            .root()
            .join("versions/.staging-interrupted/gridstator"),
        b"partial",
    )
    .unwrap();
    fs::write(store.root().join("state.json.interrupted"), b"{partial").unwrap();
    assert_eq!(store.state().unwrap().unwrap().current, "old");
    assert!(store.activate(".staging-interrupted", None).is_err());
    assert_eq!(store.state().unwrap().unwrap().current, "old");
}

#[test]
fn tampered_installed_payload_cannot_be_activated() {
    let temp = TempDir::new().unwrap();
    let store = prepare_store(&temp);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(store.executable("candidate").unwrap())
        .unwrap();
    file.write_all(b"tampered").unwrap();
    assert!(
        store
            .activate("candidate", Some(Duration::from_secs(1)))
            .is_err()
    );
    assert_eq!(store.state().unwrap().unwrap().current, "old");
}

#[tokio::test]
async fn invalid_pending_payload_rolls_back_before_it_can_execute() {
    let temp = TempDir::new().unwrap();
    let store = prepare_store(&temp);
    store
        .activate("candidate", Some(Duration::from_secs(5)))
        .unwrap();
    fs::write(
        store.executable("candidate").unwrap(),
        b"corrupted after activation",
    )
    .unwrap();
    let trace = temp.path().join("trace");
    let script = script(
        &temp,
        "echo \"$GRIDSTATOR_INSTALL_VERSION\" > \"$TRACE\"\nexit 0\n",
    );
    let outcome = supervise(&store, options(&script, &trace)).await.unwrap();
    assert_eq!(outcome.build_id, "old");
    assert_eq!(
        outcome.last_rollback.unwrap().reason,
        RollbackReason::Crashed
    );
    assert_eq!(fs::read_to_string(trace).unwrap(), "old\n");
}

fn target_header(target: &str) -> Vec<u8> {
    let mut bytes = vec![0u8; 128];
    match target {
        "linux/amd64" | "linux/arm64" => {
            bytes[..4].copy_from_slice(b"\x7fELF");
            bytes[4] = 2;
            bytes[5] = 1;
            let architecture = if target == "linux/amd64" {
                62u16
            } else {
                183u16
            };
            bytes[18..20].copy_from_slice(&architecture.to_le_bytes());
        }
        "darwin/amd64" | "darwin/arm64" => {
            bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            let architecture = if target == "darwin/amd64" {
                0x0100_0007u32
            } else {
                0x0100_000cu32
            };
            bytes[4..8].copy_from_slice(&architecture.to_le_bytes());
        }
        _ => panic!("unsupported test target"),
    }
    bytes
}

fn release_fixture(temp: &TempDir) -> (ReleaseManifest, BTreeMap<String, Vec<u8>>) {
    // These are parsing fixtures, not cross-platform execution evidence. Real OS/architecture
    // runtime validation remains a separate release gate.
    let mut artifacts = BTreeMap::new();
    let mut files = BTreeMap::new();
    for target in SUPPORTED_TARGETS {
        let file = format!("gridstator-{}", target.replace('/', "-"));
        let path = temp.path().join(&file);
        let bytes = target_header(target);
        fs::write(&path, &bytes).unwrap();
        artifacts.insert(
            target.into(),
            ReleaseArtifact {
                archive: FileDigest::from_path(&file, &path).unwrap(),
                executable: FileDigest::from_path("gridstator", &path).unwrap(),
            },
        );
        files.insert(file, bytes);
    }
    (
        ReleaseManifest {
            schema_version: MANIFEST_VERSION,
            build_id: "common-release-before-build".into(),
            version: "2.0.0".into(),
            source_revision: "fixture-only".into(),
            protocol: ProtocolCompatibility::default(),
            artifacts,
        },
        files,
    )
}

#[test]
fn mirror_bootstrap_verifies_another_target_without_executing_it() {
    let temp = TempDir::new().unwrap();
    let (manifest, _) = release_fixture(&temp);
    let mirror = ArtifactMirror::new(temp.path().join("controller mirror"));
    let bytes = manifest.to_bytes().unwrap();
    mirror
        .cache_manifest(&bytes, &sha256_bytes(&bytes))
        .unwrap();
    let target = if host_target().unwrap().starts_with("linux/") {
        "darwin/arm64"
    } else {
        "linux/arm64"
    };
    let source = temp
        .path()
        .join(&manifest.target(target).unwrap().archive.file);
    let path = mirror.cache_artifact(&manifest, target, &source).unwrap();
    assert_eq!(fs::read(&path).unwrap(), target_header(target));
    assert_eq!(mirror.verified_artifact(&manifest, target).unwrap(), path);
    let mut conflicting = manifest.clone();
    conflicting.source_revision = "other-source".into();
    let bytes = conflicting.to_bytes().unwrap();
    assert!(
        mirror
            .cache_manifest(&bytes, &sha256_bytes(&bytes))
            .is_err()
    );
    assert_eq!(mirror.manifest(&manifest.build_id).unwrap(), manifest);
}

async fn http_fixture(routes: BTreeMap<String, Vec<u8>>) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = std::sync::Arc::new(routes);
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let count = socket.read(&mut chunk).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                    assert!(request.len() < 8192);
                }
                let first = std::str::from_utf8(&request)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap();
                let path = first
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .split('?')
                    .next()
                    .unwrap();
                let (status, body) = routes
                    .get(path)
                    .map(|bytes| ("200 OK", bytes.as_slice()))
                    .unwrap_or(("404 Not Found", b"missing"));
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(body).await.unwrap();
            });
        }
    });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn official_discovery_filters_incomplete_releases_and_bootstraps_the_verified_mirror() {
    let temp = TempDir::new().unwrap();
    let (manifest, files) = release_fixture(&temp);
    let bytes = manifest.to_bytes().unwrap();
    let mut checksums = format!("{}  {RELEASE_MANIFEST_NAME}\n", sha256_bytes(&bytes));
    let mut assets = vec![
        serde_json::json!({"name":RELEASE_MANIFEST_NAME}),
        serde_json::json!({"name":"SHA256SUMS"}),
    ];
    let mut routes = BTreeMap::new();
    routes.insert(format!("/download/v2.0.0/{RELEASE_MANIFEST_NAME}"), bytes);
    for (filename, body) in files {
        checksums.push_str(&format!("{}  {filename}\n", sha256_bytes(&body)));
        assets.push(serde_json::json!({"name":filename}));
        routes.insert(format!("/download/v2.0.0/{filename}"), body);
    }
    routes.insert("/download/v2.0.0/SHA256SUMS".into(), checksums.into_bytes());
    let stable = serde_json::json!({"tag_name":"v2.0.0","name":"Native release","body":"notes","published_at":"2026-11-10T00:00:00Z","draft":false,"prerelease":false,"assets":assets});
    let mut preview = stable.clone();
    preview["tag_name"] = "v2.1.0-preview.1".into();
    let mut incomplete = stable.clone();
    incomplete["tag_name"] = "v2.0.1".into();
    incomplete["assets"] =
        serde_json::json!([{"name":"gridstator-2.0.1.tar.gz"},{"name":"SHA256SUMS"}]);
    routes.insert(
        "/api/".into(),
        serde_json::to_vec(&vec![incomplete, preview, stable.clone()]).unwrap(),
    );
    routes.insert(
        "/api/tags/v2.0.0".into(),
        serde_json::to_vec(&stable).unwrap(),
    );
    let (base, server) = http_fixture(routes).await;
    let source = OfficialReleases::with_base_urls(
        &format!("{base}/api/"),
        &format!("{base}/download/"),
        &format!("{base}/web/"),
    )
    .unwrap();
    assert_eq!(
        source
            .list(false)
            .await
            .unwrap()
            .iter()
            .map(|release| release.tag.as_str())
            .collect::<Vec<_>>(),
        ["v2.0.0"]
    );
    assert_eq!(source.list(true).await.unwrap().len(), 2);
    let selected = source.fetch_manifest("v2.0.0").await.unwrap();
    assert_eq!(selected.manifest, manifest);
    let mirror = ArtifactMirror::new(temp.path().join("fresh controller mirror"));
    let path = source
        .mirror_artifact(&selected, "darwin/arm64", &mirror)
        .await
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), target_header("darwin/arm64"));
    // A second request succeeds from the immutable cache even after the distribution server stops.
    server.abort();
    assert_eq!(
        source
            .mirror_artifact(&selected, "darwin/arm64", &mirror)
            .await
            .unwrap(),
        path
    );
    assert!(source.fetch_manifest("../arbitrary").await.is_err());
}

fn installer_command(temp: &TempDir, downloads: &Path, prefix: &Path) -> std::process::Command {
    let tools = temp.path().join("fake download tools");
    fs::create_dir_all(&tools).unwrap();
    let curl = tools.join("curl");
    fs::write(
        &curl,
        r#"#!/bin/sh
set -eu
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output=$2; shift 2 ;;
    --proto|--proto-redir|--max-filesize|--connect-timeout|--max-time) shift 2 ;;
    --fail|--location|--silent|--show-error|--tlsv1.2) shift ;;
    https://fixtures.invalid/*) url=$1; shift ;;
    *) exit 9 ;;
  esac
done
[ -n "$output" ] && [ -n "$url" ]
cp "$FIXTURE_DOWNLOADS/${url##*/}" "$output"
"#,
    )
    .unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    for runtime in ["node", "npm", "rustc", "cargo", "python", "python3", "jq"] {
        let path = tools.join(runtime);
        fs::write(
            &path,
            "#!/bin/sh\necho forbidden-runtime >> \"$RUNTIME_USED\"\nexit 99\n",
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/install-native.sh");
    let mut command = std::process::Command::new("sh");
    command
        .arg(script)
        .args([
            "--tag",
            &format!("v{}", gridstator::VERSION),
            "--base-url",
            "https://fixtures.invalid/download",
            "--prefix",
        ])
        .arg(prefix);
    command.env(
        "PATH",
        format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
    );
    command
        .env("FIXTURE_DOWNLOADS", downloads)
        .env("RUNTIME_USED", temp.path().join("runtime-was-used"));
    command
}

fn installer_release_fixture(temp: &TempDir, kind: &str) -> PathBuf {
    let original = Path::new(env!("CARGO_BIN_EXE_gridstator"));
    let binary = temp.path().join("installer-native");
    fs::copy(original, &binary).unwrap();
    // Exercise the actual native helper without repeatedly hashing/packing hundreds of MiB of
    // development symbols. Stripping is a Linux fixture optimization, never a test prerequisite.
    #[cfg(target_os = "linux")]
    if !std::process::Command::new("strip")
        .arg("--strip-debug")
        .arg(&binary)
        .status()
        .is_ok_and(|status| status.success())
    {
        fs::copy(original, &binary).unwrap();
    }
    let info = std::process::Command::new(&binary)
        .args(["build-info", "--json"])
        .output()
        .unwrap();
    assert!(
        info.status.success(),
        "build-info failed: {}",
        String::from_utf8_lossy(&info.stderr)
    );
    let info: serde_json::Value =
        serde_json::from_slice(&info.stdout).expect("native build-info JSON contract");
    let build_id = info["buildId"].as_str().expect("native build ID");
    assert_eq!(info["version"], gridstator::VERSION);
    assert_eq!(info["target"], host_target().unwrap());
    let downloads = temp.path().join("downloads");
    fs::create_dir(&downloads).unwrap();
    let archive_name = format!(
        "gridstator-{}-{}.tar.gz",
        gridstator::VERSION,
        host_target().unwrap().replace('/', "-")
    );
    let archive = downloads.join(&archive_name);
    let executable = fs::read(&binary).unwrap();
    if kind == "symlink" {
        tar_archive(&archive, &[("gridstator", b"", tar::EntryType::Symlink)]);
    } else {
        tar_archive(
            &archive,
            &[("gridstator", &executable, tar::EntryType::Regular)],
        );
    }
    let manifest = manifest(build_id, &archive, &archive_name, &binary, "gridstator");
    let manifest_bytes = manifest.to_bytes().unwrap();
    fs::write(downloads.join(RELEASE_MANIFEST_NAME), &manifest_bytes).unwrap();
    let checksums = format!(
        "{}  {RELEASE_MANIFEST_NAME}\n{}  {archive_name}\n",
        sha256_bytes(&manifest_bytes),
        sha256_bytes(&fs::read(&archive).unwrap())
    );
    fs::write(downloads.join("SHA256SUMS"), checksums).unwrap();
    if kind == "corrupt" {
        fs::OpenOptions::new()
            .append(true)
            .open(archive)
            .unwrap()
            .write_all(b"corruption")
            .unwrap();
    }
    downloads
}

#[test]
fn shell_installer_uses_a_pinned_offline_bundle_without_downloading() {
    let temp = TempDir::new().unwrap();
    let downloads = installer_release_fixture(&temp, "regular");
    let prefix = temp.path().join("offline prefix with spaces");
    let digest = sha256_bytes(&fs::read(downloads.join(RELEASE_MANIFEST_NAME)).unwrap());
    let manifest = ReleaseManifest::read(downloads.join(RELEASE_MANIFEST_NAME)).unwrap();
    let archive_digest = &manifest
        .target(host_target().unwrap())
        .unwrap()
        .archive
        .sha256;
    let mut command = installer_command(&temp, &downloads, &prefix);
    command.args(["--bundle-dir"]).arg(&downloads).args([
        "--manifest-sha256",
        &digest,
        "--archive-sha256",
        archive_digest,
    ]);
    let curl = temp.path().join("fake download tools/curl");
    fs::write(
        &curl,
        "#!/bin/sh\necho offline-install-attempted-download >&2\nexit 99\n",
    )
    .unwrap();
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(prefix.join("bin/gridstator")).unwrap(),
        fs::read(temp.path().join("installer-native")).unwrap()
    );
    assert!(!temp.path().join("runtime-was-used").exists());
    let archive = downloads.join(
        &manifest
            .target(host_target().unwrap())
            .unwrap()
            .archive
            .file,
    );
    let mut executable = fs::read(temp.path().join("installer-native")).unwrap();
    executable.extend_from_slice(b"modified executable");
    tar_archive(
        &archive,
        &[("gridstator", &executable, tar::EntryType::Regular)],
    );
    let checksums = downloads.join("SHA256SUMS");
    let old = fs::read_to_string(&checksums).unwrap();
    fs::write(
        &checksums,
        old.replace(archive_digest, &sha256_bytes(&fs::read(&archive).unwrap())),
    )
    .unwrap();
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("trusted archive checksum mismatch"));
    // Rewriting the manifest and its adjacent checksum cannot replace the digest pinned by the app.
    let path = downloads.join(RELEASE_MANIFEST_NAME);
    fs::write(&path, b"changed manifest").unwrap();
    let checksums = downloads.join("SHA256SUMS");
    let old = fs::read_to_string(&checksums).unwrap();
    fs::write(
        &checksums,
        old.replace(&digest, &sha256_bytes(b"changed manifest")),
    )
    .unwrap();
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("manifest checksum mismatch"));
}

#[test]
fn shell_installer_uses_verified_native_helper_without_language_runtime_or_service_changes() {
    let temp = TempDir::new().unwrap();
    let downloads = installer_release_fixture(&temp, "regular");
    let prefix = temp.path().join("prefix with spaces");
    let result = installer_command(&temp, &downloads, &prefix)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read(prefix.join("bin/gridstator")).unwrap(),
        fs::read(temp.path().join("installer-native")).unwrap()
    );
    let metadata = prefix
        .join("share/gridstator/releases")
        .join(format!("v{}", gridstator::VERSION));
    assert!(metadata.join(RELEASE_MANIFEST_NAME).exists());
    assert!(metadata.join("SHA256SUMS").exists());
    assert!(!prefix.join("share/gridstator/.installer-lock").exists());
    assert!(!temp.path().join("runtime-was-used").exists());
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("No")
            || String::from_utf8_lossy(&result.stdout).contains("explicit setup commands")
    );
}

#[test]
fn shell_installer_checksum_and_symlink_failures_preserve_the_existing_cli() {
    for kind in ["corrupt", "symlink"] {
        let temp = TempDir::new().unwrap();
        let downloads = installer_release_fixture(&temp, kind);
        let prefix = temp.path().join("prefix");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin/gridstator"), "old installation").unwrap();
        let result = installer_command(&temp, &downloads, &prefix)
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "{kind} installer fixture unexpectedly succeeded"
        );
        assert_eq!(
            fs::read_to_string(prefix.join("bin/gridstator")).unwrap(),
            "old installation"
        );
        assert!(!prefix.join("share/gridstator/releases").exists());
        assert!(!temp.path().join("runtime-was-used").exists());
    }
}

#[test]
fn shell_installer_if_missing_verifies_existing_bytes_without_replacing_them() {
    let temp = TempDir::new().unwrap();
    let downloads = installer_release_fixture(&temp, "regular");
    let prefix = temp.path().join("prefix");
    fs::create_dir_all(prefix.join("bin")).unwrap();
    let executable = prefix.join("bin/gridstator");
    let execution_marker = temp.path().join("candidate-was-executed");
    let existing = format!(
        "#!/bin/sh\nprintf unsafe > '{}'\n",
        execution_marker.display()
    );
    fs::write(&executable, &existing).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let result = installer_command(&temp, &downloads, &prefix)
        .arg("--if-missing")
        .output()
        .unwrap();
    assert!(
        !result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read_to_string(&executable).unwrap(), existing);
    assert!(
        !execution_marker.exists(),
        "the unverified existing CLI must never execute"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("left unchanged"));
    assert!(!prefix.join("share/gridstator/.installer-lock").exists());
    assert!(
        !prefix
            .join("share/gridstator/releases")
            .join(format!("v{}", gridstator::VERSION))
            .exists()
    );

    // Manual installation keeps its existing explicit replacement behavior.
    let replaced = installer_command(&temp, &downloads, &prefix)
        .output()
        .unwrap();
    assert!(
        replaced.status.success(),
        "{}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    assert_eq!(
        fs::read(&executable).unwrap(),
        fs::read(temp.path().join("installer-native")).unwrap()
    );
    // A matching binary may have been copied there by another window or by hand. Validate it
    // using the trusted downloaded helper and add the metadata needed by service installation.
    let metadata = prefix
        .join("share/gridstator/releases")
        .join(format!("v{}", gridstator::VERSION));
    fs::remove_dir_all(&metadata).unwrap();
    let verified = installer_command(&temp, &downloads, &prefix)
        .arg("--if-missing")
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(String::from_utf8_lossy(&verified.stdout).contains("Verified existing"));
    assert_eq!(
        fs::read(&executable).unwrap(),
        fs::read(temp.path().join("installer-native")).unwrap()
    );
    assert!(metadata.join(RELEASE_MANIFEST_NAME).exists());
    assert!(metadata.join("SHA256SUMS").exists());
    assert!(metadata.join("verification.json").exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn supervisor_cli_forwards_shutdown_to_its_owned_payload_group() {
    let temp = TempDir::new().unwrap();
    let store = InstallStore::new(temp.path().join("store"));
    install_fixture(&store, "old");
    store.activate("old", None).unwrap();
    let trace = temp.path().join("signal trace");
    let ready = temp.path().join("ready");
    let script = script(
        &temp,
        "trap 'echo received > \"$TRACE\"; exit 0' TERM\necho ready > \"$READY\"\nwhile :; do sleep 1; done\n",
    );
    let mut supervisor = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"))
        .arg("supervise")
        .arg("--install-root")
        .arg(store.root())
        .arg("--")
        .arg(script)
        .env("TRACE", &trace)
        .env("READY", &ready)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for_file(&ready).await;
    let pid = supervisor.id().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let status = tokio::time::timeout(Duration::from_secs(5), supervisor.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(128 + libc::SIGTERM));
    assert_eq!(fs::read_to_string(trace).unwrap(), "received\n");
}
