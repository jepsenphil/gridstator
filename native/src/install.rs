//! Verified, immutable payloads and a stable native service supervisor.
//!
//! Callers must finish draining/quiescing before activation. No confirmation timer runs while an
//! update downloads or drains: the supervisor starts it immediately before launching the candidate.

use crate::release::{
    MAX_ARTIFACT_BYTES, ReleaseManifest, hash_file, host_target, invalid, validate_build_id,
    validate_relative_path, verify_executable_target,
};
use flate2::read::GzDecoder;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tempfile::{Builder, NamedTempFile};
use tokio::process::{Child, Command};

pub const RESTART_EXIT_CODE: i32 = 75;
pub const INSTALL_STATE_VERSION: u32 = 1;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const EXECUTABLE: &str = "gridstator";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingActivation {
    pub build_id: String,
    pub previous: String,
    pub attempts: u32,
    pub confirm_within_ms: u64,
    /// Persisted on first launch, not when the caller activates a drained/downloaded update.
    pub first_launch_at: Option<u64>,
    pub deadline: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RollbackReason {
    Crashed,
    TimedOut,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackRecord {
    pub from: String,
    pub to: String,
    pub at: u64,
    pub reason: RollbackReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallState {
    pub schema_version: u32,
    pub current: String,
    pub previous: Option<String>,
    pub pending: Option<PendingActivation>,
    pub last_rollback: Option<RollbackRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledRelease {
    pub build_id: String,
    pub target: String,
    pub executable: PathBuf,
    /// Digest of the canonical stored manifest, independent of downloaded document formatting.
    pub manifest_sha256: String,
}

#[derive(Clone, Debug)]
pub struct InstallStore {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthyConfirmation {
    pub confirmed: bool,
    pub supervisor_refreshed: bool,
    /// Refresh failure never undoes a payload whose health confirmation was persisted.
    pub refresh_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VerifiedRunningPayload {
    pub installed: InstalledRelease,
    pub manifest: ReleaseManifest,
    pub state: InstallState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunningVerificationKey {
    root: PathBuf,
    executable: PathBuf,
    manifest: String,
    stamp: String,
}

fn executable_stamp(metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        )
    }
    #[cfg(not(unix))]
    {
        format!(
            "{}:{:?}:{:?}",
            metadata.len(),
            metadata.modified(),
            metadata.created()
        )
    }
}

impl InstallStore {
    /// Construct a handle without touching disk. Mutations create private directories as needed.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let store = Self::new(root);
        store.ensure()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn supervisor_path(&self) -> PathBuf {
        self.root.join("supervisor")
    }

    pub fn executable(&self, build_id: &str) -> io::Result<PathBuf> {
        validate_build_id(build_id)?;
        Ok(self.root.join("versions").join(build_id).join(EXECUTABLE))
    }

    pub fn manifest(&self, build_id: &str) -> io::Result<ReleaseManifest> {
        validate_build_id(build_id)?;
        ReleaseManifest::read(
            self.root
                .join("versions")
                .join(build_id)
                .join("release.json"),
        )
    }

    pub fn state(&self) -> io::Result<Option<InstallState>> {
        let _lock = self.lock()?;
        self.read_state()
    }

    /// Verify the archive *and* selected executable before making a version visible. Only regular
    /// archive entries are allowed; links, traversal, duplicates, and oversized members fail closed.
    /// This does not activate the payload or start/modify a service.
    pub fn install_verified(
        &self,
        manifest: &ReleaseManifest,
        target: &str,
        archive_path: impl AsRef<Path>,
    ) -> io::Result<InstalledRelease> {
        manifest.validate()?;
        if target != host_target()? {
            return Err(invalid(
                "cannot install a payload for a different host target",
            ));
        }
        let artifact = manifest.target(target)?;
        let _lock = self.lock()?;
        artifact.archive.verify(archive_path.as_ref())?;
        let dest = self.root.join("versions").join(&manifest.build_id);
        if fs::symlink_metadata(&dest).is_ok() {
            let existing = self.verify_installed(&manifest.build_id)?;
            if self.manifest(&manifest.build_id)? != *manifest || existing.target != target {
                return Err(invalid(
                    "build ID already belongs to another immutable release",
                ));
            }
            return Ok(existing);
        }
        let staging = Builder::new()
            .prefix(".staging-")
            .tempdir_in(self.root.join("versions"))?;
        let binary = staging.path().join(EXECUTABLE);
        if artifact.archive.file.ends_with(".tar.gz") || artifact.archive.file.ends_with(".tgz") {
            extract_executable(
                archive_path.as_ref(),
                &artifact.executable.file,
                &binary,
                artifact.executable.size,
            )?;
        } else {
            let mut source = File::open(archive_path.as_ref())?;
            let mut out = private_file(&binary)?;
            let copied = io::copy(
                &mut Read::by_ref(&mut source).take(artifact.executable.size + 1),
                &mut out,
            )?;
            if copied != artifact.executable.size {
                return Err(invalid("raw executable size differs from its manifest"));
            }
            out.sync_all()?;
        }
        self.finish_staging(manifest, target, staging.path())
    }

    /// Adopt an already downloaded/installed CLI using its matching trusted manifest. This is the
    /// first-service bootstrap path when the installer retained the manifest but removed its tgz.
    /// It verifies the executable layer; distribution archive verification belongs to that earlier
    /// download. The caller also checks the running binary's embedded build ID/version.
    pub fn install_verified_executable(
        &self,
        manifest: &ReleaseManifest,
        target: &str,
        executable: impl AsRef<Path>,
    ) -> io::Result<InstalledRelease> {
        manifest.validate()?;
        if target != host_target()? {
            return Err(invalid(
                "cannot install a payload for a different host target",
            ));
        }
        let artifact = manifest.target(target)?;
        artifact.executable.verify(executable.as_ref())?;
        verify_executable_target(executable.as_ref(), target)?;
        let _lock = self.lock()?;
        let dest = self.root.join("versions").join(&manifest.build_id);
        if fs::symlink_metadata(&dest).is_ok() {
            let existing = self.verify_installed(&manifest.build_id)?;
            if self.manifest(&manifest.build_id)? != *manifest || existing.target != target {
                return Err(invalid(
                    "build ID already belongs to another immutable release",
                ));
            }
            return Ok(existing);
        }
        let staging = Builder::new()
            .prefix(".staging-")
            .tempdir_in(self.root.join("versions"))?;
        let mut source = File::open(executable.as_ref())?;
        let mut destination = private_file(&staging.path().join(EXECUTABLE))?;
        let copied = io::copy(
            &mut Read::by_ref(&mut source).take(artifact.executable.size + 1),
            &mut destination,
        )?;
        if copied != artifact.executable.size {
            return Err(invalid("executable changed size during adoption"));
        }
        destination.sync_all()?;
        self.finish_staging(manifest, target, staging.path())
    }

    fn finish_staging(
        &self,
        manifest: &ReleaseManifest,
        target: &str,
        staging: &Path,
    ) -> io::Result<InstalledRelease> {
        let binary = staging.join(EXECUTABLE);
        manifest.target(target)?.executable.verify(&binary)?;
        verify_executable_target(&binary, target)?;
        set_executable(&binary)?;
        File::open(&binary)?.sync_all()?;
        let stored = InstalledRelease {
            build_id: manifest.build_id.clone(),
            target: target.to_owned(),
            executable: self.executable(&manifest.build_id)?,
            manifest_sha256: manifest.sha256()?,
        };
        write_new_file(&staging.join("release.json"), &manifest.to_bytes()?)?;
        write_new_file(&staging.join("installed.json"), &json_bytes(&stored)?)?;
        sync_directory(staging)?;
        let dest = self.root.join("versions").join(&manifest.build_id);
        fs::rename(staging, &dest)?;
        sync_directory(&self.root.join("versions"))?;
        // TempDir's old path no longer exists; its cleanup cannot remove the adopted payload.
        Ok(stored)
    }

    /// Copy a verified running/release executable to the stable supervisor location. Updating this
    /// location is explicit, atomic, and serialized with payload mutations. The supervisor's own
    /// state schema compatibility must be checked by the caller before replacing an older launcher.
    pub fn install_supervisor(
        &self,
        executable: impl AsRef<Path>,
        replace: bool,
    ) -> io::Result<PathBuf> {
        verify_executable_target(executable.as_ref(), host_target()?)?;
        let _lock = self.lock()?;
        let destination = self.supervisor_path();
        if destination.exists() && !replace {
            return Ok(destination);
        }
        self.replace_supervisor(executable.as_ref(), None)
    }

    fn replace_supervisor(
        &self,
        executable: &Path,
        expected: Option<&crate::release::FileDigest>,
    ) -> io::Result<PathBuf> {
        let destination = self.supervisor_path();
        let mut staged = NamedTempFile::new_in(&self.root)?;
        let mut input = File::open(executable)?;
        io::copy(&mut input, staged.as_file_mut())?;
        if let Some(expected) = expected {
            expected.verify(staged.path())?;
        }
        set_executable(staged.path())?;
        staged.as_file().sync_all()?;
        staged.persist(&destination).map_err(|e| e.error)?;
        sync_directory(&self.root)?;
        Ok(destination)
    }

    /// Activate after the controller has drained/quiesced. Existing pending trials cannot be
    /// overwritten. A first installation has no rollback target and is therefore not provisional.
    pub fn activate(
        &self,
        build_id: &str,
        confirm_within: Option<Duration>,
    ) -> io::Result<InstallState> {
        validate_build_id(build_id)?;
        let timeout = confirm_within.map(duration_ms).transpose()?;
        if timeout == Some(0) {
            return Err(invalid("confirmation timeout must be positive"));
        }
        let _lock = self.lock()?;
        self.verify_installed(build_id)?;
        let previous = self.read_state()?;
        if let Some(state) = &previous {
            if state.current == build_id {
                return Ok(state.clone());
            }
            if state.pending.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "an update is already pending confirmation",
                ));
            }
            self.verify_installed(&state.current)?;
        }
        let prior_id = previous.as_ref().map(|s| s.current.clone());
        let state = InstallState {
            schema_version: INSTALL_STATE_VERSION,
            current: build_id.to_owned(),
            previous: prior_id.clone(),
            pending: prior_id.and_then(|prior| {
                timeout.map(|ms| PendingActivation {
                    build_id: build_id.to_owned(),
                    previous: prior,
                    attempts: 0,
                    confirm_within_ms: ms,
                    first_launch_at: None,
                    deadline: None,
                })
            }),
            last_rollback: previous.and_then(|s| s.last_rollback),
        };
        self.write_state(&state)?;
        Ok(state)
    }

    /// The payload calls this only after its controller/API health contract is satisfied.
    pub fn confirm(&self, build_id: &str) -> io::Result<bool> {
        validate_build_id(build_id)?;
        let _lock = self.lock()?;
        let Some(mut state) = self.read_state()? else {
            return Ok(false);
        };
        let Some(pending) = &state.pending else {
            return Ok(false);
        };
        if state.current != build_id || pending.build_id != build_id {
            return Ok(false);
        }
        let deadline = pending
            .deadline
            .ok_or_else(|| invalid("candidate has not been launched by the supervisor"))?;
        if now_ms()? >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "candidate confirmation deadline elapsed",
            ));
        }
        self.verify_installed(build_id)?;
        state.pending = None;
        self.write_state(&state)?;
        Ok(true)
    }

    /// Called after authenticated runtime reconciliation, never merely after process launch.
    /// Identity comes from this executable and its verified immutable installed location.
    pub fn confirm_running_payload(&self) -> io::Result<HealthyConfirmation> {
        let _lock = self.lock()?;
        let id = crate::BUILD_ID;
        let running = fs::canonicalize(std::env::current_exe()?)?;
        let verified = self.verify_running_payload_locked()?;
        let manifest = verified.manifest;
        let expected = &manifest.target(host_target()?)?.executable;
        let mut state = verified.state;
        if state.current != id {
            if state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.previous == id)
            {
                return Ok(HealthyConfirmation {
                    confirmed: false,
                    supervisor_refreshed: false,
                    refresh_error: None,
                });
            }
            return Err(invalid(
                "running native payload is not active in this installation",
            ));
        }
        let confirmed = if let Some(pending) = &state.pending {
            if pending.build_id != id {
                return Err(invalid(
                    "pending native payload does not match the running executable",
                ));
            }
            let deadline = pending
                .deadline
                .ok_or_else(|| invalid("candidate has not been launched by the supervisor"))?;
            if now_ms()? >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "candidate confirmation deadline elapsed",
                ));
            }
            state.pending = None;
            self.write_state(&state)?;
            true
        } else {
            false
        };
        if expected.verify(self.supervisor_path()).is_ok() {
            return Ok(HealthyConfirmation {
                confirmed,
                supervisor_refreshed: false,
                refresh_error: None,
            });
        }
        match self.replace_supervisor(&running, Some(expected)) {
            Ok(_) => Ok(HealthyConfirmation {
                confirmed,
                supervisor_refreshed: true,
                refresh_error: None,
            }),
            Err(error) => Ok(HealthyConfirmation {
                confirmed,
                supervisor_refreshed: false,
                refresh_error: Some(format!(
                    "healthy native payload {id} remains confirmed, but its stable supervisor could not be refreshed: {error}"
                )),
            }),
        }
    }

    pub fn verify_installed(&self, build_id: &str) -> io::Result<InstalledRelease> {
        let (installed, manifest) = self.installed_metadata(build_id)?;
        manifest
            .target(&installed.target)?
            .executable
            .verify(&installed.executable)?;
        verify_executable_target(&installed.executable, &installed.target)?;
        Ok(installed)
    }

    /// Read-only identity/capability check. Successful immutable byte verification is cached
    /// only while the selected manifest and kernel file identity/timestamps remain unchanged.
    /// Callers on an async runtime must still offload filesystem/lock waits.
    pub fn verify_running_payload(&self) -> io::Result<VerifiedRunningPayload> {
        let _lock = self.lock()?;
        self.verify_running_payload_locked()
    }
    fn verify_running_payload_locked(&self) -> io::Result<VerifiedRunningPayload> {
        static VERIFIED: std::sync::OnceLock<std::sync::Mutex<Option<RunningVerificationKey>>> =
            std::sync::OnceLock::new();
        let id = crate::BUILD_ID;
        let (installed, manifest) = self.installed_metadata(id)?;
        let running = fs::canonicalize(std::env::current_exe()?)?;
        if running != fs::canonicalize(&installed.executable)? {
            return Err(invalid(
                "running executable is not the verified installed native payload",
            ));
        }
        if manifest.version != crate::VERSION || manifest.build_id != id {
            return Err(invalid(
                "running executable embedded identity does not match its installed release",
            ));
        }
        let expected = &manifest.target(host_target()?)?.executable;
        let metadata = fs::symlink_metadata(&installed.executable)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != expected.size
        {
            return Err(invalid(
                "running native payload is not a regular file with the expected size",
            ));
        }
        let key = RunningVerificationKey {
            root: fs::canonicalize(&self.root)?,
            executable: running.clone(),
            manifest: installed.manifest_sha256.clone(),
            stamp: executable_stamp(&metadata),
        };
        let mut cache = VERIFIED
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .map_err(|_| invalid("running native payload verification cache is unavailable"))?;
        if cache.as_ref() != Some(&key) {
            expected.verify(&running)?;
            verify_executable_target(&running, host_target()?)?;
            if executable_stamp(&fs::symlink_metadata(&installed.executable)?) != key.stamp {
                return Err(invalid(
                    "running native payload changed while its identity was verified",
                ));
            }
            *cache = Some(key);
        }
        drop(cache);
        let state = self
            .read_state()?
            .ok_or_else(|| invalid("managed installation has no active native payload"))?;
        if state.current != id
            && !state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.previous == id && pending.build_id == state.current)
        {
            return Err(invalid(
                "running native payload is not active in this installation",
            ));
        }
        Ok(VerifiedRunningPayload {
            installed,
            manifest,
            state,
        })
    }
    fn installed_metadata(
        &self,
        build_id: &str,
    ) -> io::Result<(InstalledRelease, ReleaseManifest)> {
        validate_build_id(build_id)?;
        let version = self.root.join("versions").join(build_id);
        let metadata = fs::symlink_metadata(&version).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    error.kind(),
                    format!(
                        "version {build_id} is missing from {}: {error}",
                        self.root.display()
                    ),
                )
            } else {
                error
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("version directory must not be a symlink"));
        }
        let bytes = fs::read(version.join("installed.json"))?;
        let installed: InstalledRelease = serde_json::from_slice(&bytes).map_err(invalid)?;
        let manifest = self.manifest(build_id)?;
        let expected_path = self.executable(build_id)?;
        if installed.build_id != build_id
            || manifest.build_id != build_id
            || installed.target != host_target()?
            || installed.executable != expected_path
            || manifest.sha256()? != installed.manifest_sha256
        {
            return Err(invalid(
                "installed release metadata does not match its version store",
            ));
        }
        Ok((installed, manifest))
    }

    fn ensure(&self) -> io::Result<()> {
        private_directory(&self.root)?;
        private_directory(&self.root.join("versions"))
    }

    fn lock(&self) -> io::Result<File> {
        self.ensure()?;
        let file = lock_file(&self.root.join("install.lock"))?;
        FileExt::lock_exclusive(&file)?;
        Ok(file)
    }

    fn read_state(&self) -> io::Result<Option<InstallState>> {
        let bytes = match fs::read(self.root.join("state.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if bytes.len() > 64 * 1024 {
            return Err(invalid("install state exceeds its size limit"));
        }
        let state: InstallState = serde_json::from_slice(&bytes).map_err(invalid)?;
        if state.schema_version != INSTALL_STATE_VERSION {
            return Err(invalid("unsupported install state version"));
        }
        validate_build_id(&state.current)?;
        if let Some(previous) = &state.previous {
            validate_build_id(previous)?;
        }
        if let Some(pending) = &state.pending {
            validate_build_id(&pending.build_id)?;
            validate_build_id(&pending.previous)?;
            if pending.build_id != state.current
                || pending.build_id == pending.previous
                || state.previous.as_deref() != Some(&pending.previous)
                || pending.confirm_within_ms == 0
                || pending.first_launch_at.is_some() != pending.deadline.is_some()
            {
                return Err(invalid("inconsistent pending activation state"));
            }
        }
        Ok(Some(state))
    }

    fn write_state(&self, state: &InstallState) -> io::Result<()> {
        atomic_write(&self.root.join("state.json"), &json_bytes(state)?)
    }

    fn prepare_launch(&self, max_attempts: u32) -> io::Result<InstalledRelease> {
        let _lock = self.lock()?;
        let mut state = self
            .read_state()?
            .ok_or_else(|| invalid("managed installation has no active release"))?;
        if let Some(pending) = &state.pending {
            let reason = if pending
                .deadline
                .is_some_and(|deadline| now_ms().is_ok_and(|now| now >= deadline))
            {
                Some(RollbackReason::TimedOut)
            } else if pending.attempts >= max_attempts {
                Some(RollbackReason::Crashed)
            } else {
                None
            };
            if let Some(reason) = reason {
                let failed = state.current.clone();
                let attempts = pending.attempts;
                self.rollback_state(&mut state, reason)?;
                if reason == RollbackReason::Crashed {
                    eprintln!(
                        "gridstator supervisor: version {failed} failed to start {attempts} times; rolled back to {}",
                        state.current
                    );
                }
            }
        }
        let installed = match self.verify_installed(&state.current) {
            Ok(installed) => installed,
            Err(error) if state.pending.is_some() => {
                eprintln!(
                    "gridstator supervisor: invalid pending payload {}: {error}",
                    state.current
                );
                self.rollback_state(&mut state, RollbackReason::Crashed)?;
                self.verify_installed(&state.current)?
            }
            Err(error) => return Err(error),
        };
        if let Some(pending) = &mut state.pending {
            if pending.first_launch_at.is_none() {
                let now = now_ms()?;
                pending.first_launch_at = Some(now);
                pending.deadline = Some(
                    now.checked_add(pending.confirm_within_ms)
                        .ok_or_else(|| invalid("confirmation deadline overflow"))?,
                );
            }
            pending.attempts = pending.attempts.saturating_add(1);
            self.write_state(&state)?;
        }
        Ok(installed)
    }

    fn rollback_if_pending(&self, build_id: &str, reason: RollbackReason) -> io::Result<bool> {
        let _lock = self.lock()?;
        let Some(mut state) = self.read_state()? else {
            return Ok(false);
        };
        if state.current != build_id
            || !state
                .pending
                .as_ref()
                .is_some_and(|p| p.build_id == build_id)
        {
            return Ok(false);
        }
        self.rollback_state(&mut state, reason)?;
        Ok(true)
    }

    fn rollback_state(&self, state: &mut InstallState, reason: RollbackReason) -> io::Result<()> {
        let failed = state.current.clone();
        let pending = state
            .pending
            .as_ref()
            .ok_or_else(|| invalid("there is no pending release to roll back"))?;
        self.verify_installed(&pending.previous)?;
        state.last_rollback = Some(RollbackRecord {
            from: state.current.clone(),
            to: pending.previous.clone(),
            at: now_ms()?,
            reason,
        });
        state.current = pending.previous.clone();
        state.previous = None;
        state.pending = None;
        self.write_state(state)?;
        if reason == RollbackReason::TimedOut {
            eprintln!(
                "gridstator supervisor: version {failed} was not confirmed in time; rolled back to {}",
                state.current
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct SupervisorOptions {
    /// Role invocation, e.g. ["agent", "start", "--data-dir", ...].
    pub args: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub max_attempts: u32,
    pub poll_interval: Duration,
    pub restart_delay: Duration,
    pub shutdown_grace: Duration,
    pub forward_signals: bool,
}

impl Default for SupervisorOptions {
    fn default() -> Self {
        Self {
            args: Vec::new(),
            environment: BTreeMap::new(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            poll_interval: Duration::from_millis(100),
            restart_delay: Duration::from_secs(1),
            shutdown_grace: Duration::from_secs(90),
            forward_signals: true,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorExit {
    pub code: i32,
    pub build_id: String,
    pub last_rollback: Option<RollbackRecord>,
}

/// The stable executable's hidden supervisor command calls this. A dedicated supervisor lock
/// prevents two launchers from owning the same installation. Payload mutations use a different,
/// short-lived lock, so health confirmation is never blocked by the supervisor waiting for a child.
pub async fn supervise(
    store: &InstallStore,
    options: SupervisorOptions,
) -> io::Result<SupervisorExit> {
    if options.max_attempts == 0 || options.poll_interval.is_zero() {
        return Err(invalid(
            "supervisor attempt limit and poll interval must be positive",
        ));
    }
    store.ensure()?;
    let supervisor_lock = lock_file(&store.root.join("supervisor.lock"))?;
    FileExt::try_lock_exclusive(&supervisor_lock).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("another supervisor owns this installation: {e}"),
        )
    })?;
    let mut signals = SignalListeners::new(options.forward_signals)?;
    loop {
        let installed = store.prepare_launch(options.max_attempts)?;
        let mut command = Command::new(&installed.executable);
        command
            .args(&options.args)
            .envs(&options.environment)
            .env("GRIDSTATOR_INSTALL_ROOT", store.root())
            .env("GRIDSTATOR_INSTALL_VERSION", &installed.build_id)
            .env("GRIDSTATOR_BUILD_ID", &installed.build_id)
            .env("GRIDSTATOR_INSTALL_TARGET", &installed.target)
            .env("GRIDSTATOR_SUPERVISED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            // Executors create their own process groups for cancellation. Give the payload its
            // own session too, so those groups remain attributable after its leader crashes.
            // SAFETY: setsid is async-signal-safe and this closure only reports its errno.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if store
                    .state()?
                    .is_some_and(|s| s.pending.is_some() && s.current == installed.build_id)
                {
                    eprintln!(
                        "gridstator supervisor: candidate {} could not start: {error}",
                        installed.build_id
                    );
                    tokio::time::sleep(options.restart_delay).await;
                    continue;
                }
                return Err(error);
            }
        };
        let mut session = PayloadSession::new(child.id())?;
        let outcome = wait_for_payload(
            store,
            &installed.build_id,
            &mut child,
            &mut signals,
            &options,
        )
        .await?;
        // Reap and clean any descendants before another payload can use the same role/data store.
        session.cleanup().await?;
        session.armed = false;
        match outcome {
            PayloadExit::Shutdown(signal) => {
                return supervisor_exit(store, &installed.build_id, 128 + signal);
            }
            PayloadExit::Switch => continue,
            PayloadExit::TimedOut => {
                store.rollback_if_pending(&installed.build_id, RollbackReason::TimedOut)?;
                continue;
            }
            PayloadExit::Exited(status) => {
                let state = store
                    .state()?
                    .ok_or_else(|| invalid("managed install state disappeared"))?;
                if state.current != installed.build_id {
                    continue;
                }
                if state
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.build_id == installed.build_id)
                {
                    tokio::time::sleep(options.restart_delay).await;
                    continue;
                }
                if status.code() == Some(RESTART_EXIT_CODE) {
                    tokio::time::sleep(options.restart_delay).await;
                    continue;
                }
                use std::os::unix::process::ExitStatusExt;
                return supervisor_exit(
                    store,
                    &installed.build_id,
                    status
                        .code()
                        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
                );
            }
        }
    }
}

fn supervisor_exit(store: &InstallStore, build_id: &str, code: i32) -> io::Result<SupervisorExit> {
    Ok(SupervisorExit {
        code,
        build_id: build_id.to_owned(),
        last_rollback: store.state()?.and_then(|s| s.last_rollback),
    })
}

enum PayloadExit {
    Exited(std::process::ExitStatus),
    Shutdown(i32),
    Switch,
    TimedOut,
}

async fn wait_for_payload(
    store: &InstallStore,
    build_id: &str,
    child: &mut Child,
    signals: &mut SignalListeners,
    options: &SupervisorOptions,
) -> io::Result<PayloadExit> {
    let mut tick = tokio::time::interval(options.poll_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            status = child.wait() => return Ok(PayloadExit::Exited(status?)),
            signal = signals.next() => {
                if signal == libc::SIGHUP { signal_group(child.id(), signal)?; continue; }
                terminate(child, options.shutdown_grace).await?;
                return Ok(PayloadExit::Shutdown(signal));
            }
            _ = tick.tick() => {
                let state = store.state()?.ok_or_else(|| invalid("managed installation lost its state"))?;
                if state.current != build_id {
                    terminate(child, options.shutdown_grace).await?;
                    return Ok(PayloadExit::Switch);
                }
                if state.pending.as_ref().and_then(|p| p.deadline).is_some_and(|deadline| now_ms().is_ok_and(|now| now >= deadline)) {
                    terminate(child, options.shutdown_grace).await?;
                    return Ok(PayloadExit::TimedOut);
                }
            }
        }
    }
}

struct SignalListeners {
    term: Option<tokio::signal::unix::Signal>,
    interrupt: Option<tokio::signal::unix::Signal>,
    hangup: Option<tokio::signal::unix::Signal>,
}

impl SignalListeners {
    fn new(enabled: bool) -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(if enabled {
            Self {
                term: Some(signal(SignalKind::terminate())?),
                interrupt: Some(signal(SignalKind::interrupt())?),
                hangup: Some(signal(SignalKind::hangup())?),
            }
        } else {
            Self {
                term: None,
                interrupt: None,
                hangup: None,
            }
        })
    }
    async fn next(&mut self) -> i32 {
        async fn receive(signal: &mut Option<tokio::signal::unix::Signal>) {
            match signal {
                Some(signal) => {
                    signal.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        }
        tokio::select! { _ = receive(&mut self.term) => libc::SIGTERM, _ = receive(&mut self.interrupt) => libc::SIGINT, _ = receive(&mut self.hangup) => libc::SIGHUP }
    }
}

struct PayloadSession {
    id: libc::pid_t,
    armed: bool,
}
impl PayloadSession {
    fn new(pid: Option<u32>) -> io::Result<Self> {
        let id = pid
            .and_then(|pid| libc::pid_t::try_from(pid).ok())
            .filter(|id| *id > 1)
            .ok_or_else(|| invalid("managed payload has no valid session ID"))?;
        // The session leader may already have exited, so getsid(id) is not required to succeed.
        // Its PID is the SID because the child ran setsid before exec.
        if id == unsafe { libc::getsid(0) } {
            return Err(invalid(
                "managed payload must not share the supervisor session",
            ));
        }
        Ok(Self { id, armed: true })
    }

    fn kill_remaining(&self) -> io::Result<bool> {
        let mut system = sysinfo::System::new();
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        let mut groups = HashSet::new();
        let mut remaining = false;
        for process in system.processes().values() {
            if process.session_id().map(|sid| sid.as_u32()) != Some(self.id as u32)
                || process.status() == sysinfo::ProcessStatus::Zombie
            {
                continue;
            }
            let Ok(pid) = libc::pid_t::try_from(process.pid().as_u32()) else {
                continue;
            };
            // Recheck the live kernel identity before signalling a discovered group. A process
            // group cannot contain members from a different session. Never target the caller's
            // session or group, and never discover ownership by executable name or ancestry.
            let group = unsafe { libc::getpgid(pid) };
            if group <= 1
                || unsafe { libc::getsid(pid) } != self.id
                || self.id == unsafe { libc::getsid(0) }
                || group == unsafe { libc::getpgrp() }
            {
                continue;
            }
            remaining = true;
            if groups.insert(group) && unsafe { libc::kill(-group, libc::SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        Ok(remaining)
    }

    async fn cleanup(&self) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if !self.kill_remaining()? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                // Do not start another payload alongside an uninterruptible owned process.
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "managed payload session did not stop",
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
impl Drop for PayloadSession {
    fn drop(&mut self) {
        if self.armed {
            // A cancelled supervisor future cannot await cleanup. Keep its installation lock
            // held while stopping the session, including children forked during the first scan.
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                match self.kill_remaining() {
                    Ok(false) => break,
                    Ok(true) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(true) => {
                        eprintln!(
                            "gridstator supervisor: payload session did not stop during cleanup"
                        );
                        break;
                    }
                    Err(error) => {
                        eprintln!(
                            "gridstator supervisor: could not clean payload session: {error}"
                        );
                        break;
                    }
                }
            }
        }
    }
}

fn signal_group(pid: Option<u32>, signal: i32) -> io::Result<()> {
    let Some(pid) = pid else {
        return Ok(());
    };
    let pid = i32::try_from(pid).map_err(invalid)?;
    // Each payload starts a new process group owned by this supervisor. Never signal the caller's
    // group or discover unrelated processes by name.
    if unsafe { libc::kill(-pid, signal) } == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

async fn terminate(child: &mut Child, grace: Duration) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let pid = child.id();
    if let Err(error) = signal_group(pid, libc::SIGTERM) {
        // Darwin reports EPERM for an unreaped exited process's empty process group. Only an
        // observed child exit makes any signalling error harmless; a live child retains it.
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        return Err(error);
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(result) => {
            result?;
        }
        Err(_) => {
            if let Err(error) = signal_group(pid, libc::SIGKILL) {
                if child.try_wait()?.is_some() {
                    return Ok(());
                }
                return Err(error);
            }
            child.wait().await?;
        }
    }
    Ok(())
}

pub(crate) fn extract_executable(
    archive_path: &Path,
    executable: &str,
    destination: &Path,
    expected_size: u64,
) -> io::Result<()> {
    let source = File::open(archive_path)?;
    let decoder = GzDecoder::new(source);
    let mut archive = tar::Archive::new(decoder);
    let mut paths = HashSet::new();
    let mut found = false;
    let mut total = 0u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let name = path
            .to_str()
            .ok_or_else(|| invalid("archive contains a non-UTF-8 path"))?;
        // A conventional leading ./ is harmless. No other dot/parent/absolute component is allowed.
        let name = name
            .strip_prefix("./")
            .unwrap_or(name)
            .trim_end_matches('/');
        if name.is_empty() && entry.header().entry_type().is_dir() {
            continue;
        }
        validate_relative_path(name)?;
        if !paths.insert(name.to_owned()) {
            return Err(invalid("archive contains a duplicate path"));
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(invalid("archive links and special files are not allowed"));
        }
        let size = entry.header().size()?;
        total = total
            .checked_add(size)
            .ok_or_else(|| invalid("archive size overflow"))?;
        if total > MAX_ARTIFACT_BYTES || paths.len() > 10_000 {
            return Err(invalid("unpacked archive exceeds its limits"));
        }
        if name == executable {
            if !kind.is_file() || size != expected_size {
                return Err(invalid("archive executable has the wrong type or size"));
            }
            let mut output = private_file(destination)?;
            let count = io::copy(&mut entry, &mut output)?;
            if count != expected_size {
                return Err(invalid("archive executable is truncated"));
            }
            output.sync_all()?;
            found = true;
        }
    }
    if !found {
        return Err(invalid("archive does not contain the announced executable"));
    }
    // Check the gzip trailer as well as tar members (tar can finish before the decoder does).
    let mut decoder = archive.into_inner();
    let mut tail = [0u8; 8192];
    let mut trailing = 0u64;
    loop {
        let count = decoder.read(&mut tail)?;
        if count == 0 {
            break;
        }
        trailing += count as u64;
        if trailing > 1024 * 1024 || tail[..count].iter().any(|byte| *byte != 0) {
            return Err(invalid("unexpected trailing archive data"));
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> io::Result<()> {
    let created = !path.exists();
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("install directory must be a real directory"));
    }
    if created {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn lock_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = private_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("state has no parent directory"))?;
    let mut staged = NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|e| e.error)?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
fn json_bytes(value: &impl Serialize) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(invalid)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn now_ms() -> io::Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(invalid)?
            .as_millis(),
    )
    .map_err(invalid)
}
fn duration_ms(duration: Duration) -> io::Result<u64> {
    u64::try_from(duration.as_millis()).map_err(invalid)
}

/// Useful for callers reporting the exact executable in native runtime metadata.
pub fn running_executable_digest() -> io::Result<(u64, String)> {
    hash_file(std::env::current_exe()?)
}

#[cfg(test)]
mod termination_tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn exited_owned_child_is_reaped_before_group_shutdown() {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 75"])
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        let mut child = command.spawn().unwrap();
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut output)
            .await
            .unwrap();
        // The pipe closes at exit. Allow the kernel to finish publishing the exited process
        // before exercising the unreaped-child path (Darwin killpg then returns EPERM).
        tokio::time::sleep(Duration::from_millis(20)).await;
        terminate(&mut child, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(
            child.try_wait().unwrap().unwrap().code(),
            Some(RESTART_EXIT_CODE)
        );
    }
}
