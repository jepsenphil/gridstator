//! Native release metadata. Build IDs are allocated before compilation; digests describe the
//! final, signed distribution and its executable independently.

use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::{Component, Path},
};

pub const RELEASE_MANIFEST_NAME: &str = "gridstator-release.json";

pub const MANIFEST_VERSION: u32 = 1;
pub const UPDATE_PROTOCOL_VERSION: u32 = 1;
pub const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const SUPPORTED_TARGETS: [&str; 4] =
    ["linux/amd64", "linux/arm64", "darwin/amd64", "darwin/arm64"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolCompatibility {
    pub api: u32,
    pub agent: u32,
    pub update: u32,
}

impl Default for ProtocolCompatibility {
    fn default() -> Self {
        Self {
            api: 1,
            agent: 1,
            update: UPDATE_PROTOCOL_VERSION,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileDigest {
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

impl FileDigest {
    pub fn from_path(file: impl Into<String>, path: impl AsRef<Path>) -> io::Result<Self> {
        let (size, sha256) = hash_file(path)?;
        Ok(Self {
            file: file.into(),
            size,
            sha256,
        })
    }

    pub fn verify(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path.as_ref())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != self.size {
            return Err(invalid(
                "artifact is not a regular file with the announced size",
            ));
        }
        let (size, digest) = hash_file(path)?;
        if size != self.size || digest != self.sha256 {
            return Err(invalid("artifact does not match its SHA-256 digest"));
        }
        Ok(())
    }

    fn validate(&self, executable: bool) -> io::Result<()> {
        validate_relative_path(&self.file)?;
        if !executable && Path::new(&self.file).components().count() != 1 {
            return Err(invalid("archive filename must be a basename"));
        }
        if self.size == 0 || self.size > MAX_ARTIFACT_BYTES {
            return Err(invalid("artifact size must be between 1 byte and 512 MiB"));
        }
        if !is_sha256(&self.sha256) {
            return Err(invalid("expected a lowercase full SHA-256 digest"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseArtifact {
    pub archive: FileDigest,
    pub executable: FileDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub build_id: String,
    pub version: String,
    pub source_revision: String,
    pub protocol: ProtocolCompatibility,
    #[serde(deserialize_with = "unique_artifacts")]
    pub artifacts: BTreeMap<String, ReleaseArtifact>,
}

impl ReleaseManifest {
    pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_bytes(&read_bounded(path.as_ref(), MAX_MANIFEST_BYTES)?)
    }

    /// `expected` must come from the caller's trusted release source, not the same untrusted body.
    pub fn read_verified(path: impl AsRef<Path>, expected: &str) -> io::Result<Self> {
        let bytes = read_bounded(path.as_ref(), MAX_MANIFEST_BYTES)?;
        if !is_sha256(expected) || sha256_bytes(&bytes) != expected {
            return Err(invalid(
                "release manifest does not match its expected digest",
            ));
        }
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(invalid("release manifest is too large"));
        }
        let value: Self = serde_json::from_slice(bytes).map_err(invalid)?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != MANIFEST_VERSION {
            return Err(invalid("unsupported release manifest version"));
        }
        validate_build_id(&self.build_id)?;
        if self.version.is_empty()
            || self.version.len() > 128
            || self.version.chars().any(char::is_control)
        {
            return Err(invalid("invalid release version"));
        }
        if self.source_revision.is_empty()
            || self.source_revision.len() > 256
            || self.source_revision.chars().any(char::is_control)
        {
            return Err(invalid("invalid source revision"));
        }
        if self.protocol.api == 0
            || self.protocol.agent == 0
            || self.protocol.update != UPDATE_PROTOCOL_VERSION
        {
            return Err(invalid("unsupported native protocol compatibility"));
        }
        if self.artifacts.is_empty()
            || self
                .artifacts
                .keys()
                .any(|key| !SUPPORTED_TARGETS.contains(&key.as_str()))
        {
            return Err(invalid(
                "release manifest has no artifacts or an unsupported target",
            ));
        }
        for artifact in self.artifacts.values() {
            artifact.archive.validate(false)?;
            artifact.executable.validate(true)?;
        }
        Ok(())
    }

    pub fn target(&self, target: &str) -> io::Result<&ReleaseArtifact> {
        self.validate()?;
        self.artifacts
            .get(target)
            .ok_or_else(|| invalid(format!("release has no artifact for {target}")))
    }

    /// Canonical bytes used by native release producers and the managed store. A downloaded raw
    /// document's checksum is checked before parsing; reserialization is not that checksum.
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(invalid)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn sha256(&self) -> io::Result<String> {
        Ok(sha256_bytes(&self.to_bytes()?))
    }
}

pub fn host_target() -> io::Result<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| invalid("this operating system/architecture is not supported"))
}

pub fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x64" | "x86_64" | "amd64") => Some("linux/amd64"),
        ("linux", "arm64" | "aarch64") => Some("linux/arm64"),
        ("macos" | "darwin", "x64" | "x86_64" | "amd64") => Some("darwin/amd64"),
        ("macos" | "darwin", "arm64" | "aarch64") => Some("darwin/arm64"),
        _ => None,
    }
}

/// Check ELF/Mach-O architecture without executing an incoming file. Universal Mach-O and scripts
/// are intentionally not single-target native release executables.
pub fn verify_executable_target(path: impl AsRef<Path>, target: &str) -> io::Result<()> {
    let mut bytes = [0u8; 64];
    let mut file = File::open(path)?;
    file.read_exact(&mut bytes)
        .map_err(|_| invalid("executable header is truncated"))?;
    let actual = if &bytes[..4] == b"\x7fELF" && bytes[4] == 2 && bytes[5] == 1 {
        match u16::from_le_bytes([bytes[18], bytes[19]]) {
            62 => "linux/amd64",
            183 => "linux/arm64",
            _ => "unsupported",
        }
    } else if bytes[..4] == [0xcf, 0xfa, 0xed, 0xfe] {
        match u32::from_le_bytes(bytes[4..8].try_into().expect("fixed header slice")) {
            0x0100_0007 => "darwin/amd64",
            0x0100_000c => "darwin/arm64",
            _ => "unsupported",
        }
    } else {
        "unsupported"
    };
    if actual != target {
        return Err(invalid(format!(
            "executable target {actual} does not match {target}"
        )));
    }
    Ok(())
}

pub fn hash_file(path: impl AsRef<Path>) -> io::Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| invalid("file size overflow"))?;
        hash.update(&buffer[..count]);
    }
    Ok((size, hex::encode(hash.finalize())))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn checksum_for(text: &str, filename: &str) -> io::Result<String> {
    let mut found = None;
    for line in text.lines() {
        let Some((digest, name)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        if name.trim_start().trim_start_matches('*') != filename {
            continue;
        }
        if !is_sha256(digest) || found.is_some() {
            return Err(invalid("missing, invalid, or ambiguous release checksum"));
        }
        found = Some(digest.to_owned());
    }
    found.ok_or_else(|| invalid(format!("release checksums do not contain {filename}")))
}

pub(crate) fn validate_build_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c))
    {
        return Err(invalid(
            "build ID must be a safe, nonempty identifier of at most 128 characters",
        ));
    }
    Ok(())
}

pub(crate) fn validate_relative_path(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || Path::new(value)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid(
            "artifact path must contain only safe relative components",
        ));
    }
    Ok(())
}

pub(crate) fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("release document exceeded its size limit"));
    }
    Ok(bytes)
}

fn unique_artifacts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, ReleaseArtifact>, D::Error> {
    struct Unique;
    impl<'de> de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, ReleaseArtifact>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a target map without duplicate keys")
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, ReleaseArtifact>()? {
                if out.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate release target"));
                }
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(Unique)
}

/// Controller-owned, immutable distribution cache. It can verify another target's executable
/// header without running it, so a fresh Linux controller can mirror Mac/ARM worker payloads.
#[derive(Clone, Debug)]
pub struct ArtifactMirror {
    root: std::path::PathBuf,
}

impl ArtifactMirror {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn cache_manifest(
        &self,
        bytes: &[u8],
        expected_sha256: &str,
    ) -> io::Result<ReleaseManifest> {
        use std::io::Write;
        if !is_sha256(expected_sha256) || sha256_bytes(bytes) != expected_sha256 {
            return Err(invalid("manifest does not match its expected digest"));
        }
        let manifest = ReleaseManifest::from_bytes(bytes)?;
        let directory = self.release_directory(&manifest.build_id)?;
        let lock = mirror_lock(&directory)?;
        let path = directory.join(RELEASE_MANIFEST_NAME);
        if path.exists() {
            let existing = read_bounded(&path, MAX_MANIFEST_BYTES)?;
            if existing != bytes {
                return Err(invalid(
                    "build ID already has a different immutable manifest",
                ));
            }
        } else {
            let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
            staged.write_all(bytes)?;
            staged.as_file().sync_all()?;
            staged.persist(&path).map_err(|error| error.error)?;
            File::open(&directory)?.sync_all()?;
        }
        drop(lock);
        Ok(manifest)
    }

    pub fn manifest(&self, build_id: &str) -> io::Result<ReleaseManifest> {
        validate_build_id(build_id)?;
        ReleaseManifest::read(self.root.join(build_id).join(RELEASE_MANIFEST_NAME))
    }

    pub fn artifact_path(
        &self,
        manifest: &ReleaseManifest,
        target: &str,
    ) -> io::Result<std::path::PathBuf> {
        let artifact = manifest.target(target)?;
        Ok(self
            .root
            .join(&manifest.build_id)
            .join(target.replace('/', "-"))
            .join(&artifact.archive.file))
    }

    /// Verify both layers and atomically import an archive without activating or executing it.
    pub fn cache_artifact(
        &self,
        manifest: &ReleaseManifest,
        target: &str,
        source: impl AsRef<Path>,
    ) -> io::Result<std::path::PathBuf> {
        let artifact = manifest.target(target)?;
        let directory = self.release_directory(&manifest.build_id)?;
        let _lock = mirror_lock(&directory)?;
        let stored = ReleaseManifest::read(directory.join(RELEASE_MANIFEST_NAME))?;
        if stored != *manifest {
            return Err(invalid(
                "artifact's manifest differs from the cached immutable release",
            ));
        }
        artifact.archive.verify(source.as_ref())?;
        verify_distribution_artifact(artifact, target, source.as_ref())?;
        let path = self.artifact_path(manifest, target)?;
        let parent = path
            .parent()
            .ok_or_else(|| invalid("mirror artifact has no directory"))?;
        private_mirror_directory(parent)?;
        if path.exists() {
            artifact.archive.verify(&path)?;
            return Ok(path);
        }
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        let mut input = File::open(source.as_ref())?;
        io::copy(&mut input, staged.as_file_mut())?;
        // Recheck the copied bytes in case the caller's source changed while being imported.
        artifact.archive.verify(staged.path())?;
        staged.as_file().sync_all()?;
        staged.persist(&path).map_err(|error| error.error)?;
        File::open(parent)?.sync_all()?;
        Ok(path)
    }

    pub fn verified_artifact(
        &self,
        manifest: &ReleaseManifest,
        target: &str,
    ) -> io::Result<std::path::PathBuf> {
        if self.manifest(&manifest.build_id)? != *manifest {
            return Err(invalid(
                "release manifest is not the cached immutable manifest",
            ));
        }
        let path = self.artifact_path(manifest, target)?;
        manifest.target(target)?.archive.verify(&path)?;
        Ok(path)
    }

    fn release_directory(&self, build_id: &str) -> io::Result<std::path::PathBuf> {
        validate_build_id(build_id)?;
        private_mirror_directory(&self.root)?;
        let directory = self.root.join(build_id);
        private_mirror_directory(&directory)?;
        Ok(directory)
    }
}

fn verify_distribution_artifact(
    artifact: &ReleaseArtifact,
    target: &str,
    source: &Path,
) -> io::Result<()> {
    if artifact.archive.file.ends_with(".tar.gz") || artifact.archive.file.ends_with(".tgz") {
        let temp = tempfile::TempDir::new()?;
        let binary = temp.path().join("gridstator");
        crate::install::extract_executable(
            source,
            &artifact.executable.file,
            &binary,
            artifact.executable.size,
        )?;
        artifact.executable.verify(&binary)?;
        verify_executable_target(binary, target)
    } else {
        artifact.executable.verify(source)?;
        verify_executable_target(source, target)
    }
}

fn private_mirror_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let created = !path.exists();
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("mirror directory must not be a symlink"));
    }
    if created {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn mirror_lock(directory: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("mirror.lock"))?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseView {
    pub tag: String,
    pub version: String,
    pub name: String,
    pub url: String,
    pub published_at: i64,
    pub prerelease: bool,
    pub notes: String,
    pub manifest_name: String,
}

#[derive(Clone, Debug)]
pub struct VerifiedRelease {
    pub release: ReleaseView,
    pub manifest: ReleaseManifest,
    pub manifest_sha256: String,
    /// Original, checksum-verified document bytes; preserve these when mirroring.
    pub manifest_bytes: Vec<u8>,
}

/// Official release transport. Authorization to install is a controller/CLI responsibility; these
/// methods only discover/download/verify and never activate a payload or control services.
#[derive(Clone, Debug)]
pub struct OfficialReleases {
    client: reqwest::Client,
    api: url::Url,
    downloads: url::Url,
    web: url::Url,
    cancellation: Option<tokio_util::sync::CancellationToken>,
    lifetime: Option<OperationLifetime>,
    cache: std::sync::Arc<tokio::sync::Mutex<Option<ReleaseCache>>>,
    installing: std::sync::Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug)]
struct ReleaseCache {
    at: std::time::Instant,
    releases: Vec<ReleaseView>,
}

#[derive(Clone)]
struct OperationLifetime(std::sync::Arc<dyn Send + Sync>);
impl std::fmt::Debug for OperationLifetime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OperationLifetime")
    }
}
impl OfficialReleases {
    pub fn github(repository: &str) -> io::Result<Self> {
        let parts = repository.split('/').collect::<Vec<_>>();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    || *part == "."
                    || *part == ".."
            })
        {
            return Err(invalid("expected a GitHub owner/repository"));
        }
        Self::with_base_urls(
            &format!("https://api.github.com/repos/{repository}/releases/"),
            &format!("https://github.com/{repository}/releases/download/"),
            &format!("https://github.com/{repository}/releases/tag/"),
        )
    }

    /// Custom origins are useful for enterprise distribution and deterministic local transport
    /// tests. Cleartext HTTP is accepted only for loopback hosts.
    pub fn with_base_urls(api: &str, downloads: &str, web: &str) -> io::Result<Self> {
        fn base(value: &str) -> io::Result<url::Url> {
            let mut url = url::Url::parse(value).map_err(invalid)?;
            let local = url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
            if !(url.scheme() == "https" || url.scheme() == "http" && local)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid(
                    "release origin must be HTTPS (or loopback HTTP for tests) without credentials/query/fragment",
                ));
            }
            if !url.path().ends_with('/') {
                url.set_path(&format!("{}/", url.path()));
            }
            Ok(url)
        }
        let client = reqwest::Client::builder()
            .user_agent(format!("gridstator/{}", crate::VERSION))
            .timeout(std::time::Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    return attempt.error("too many release redirects");
                }
                let destination = attempt.url();
                let loopback = destination.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                });
                let cleartext_test = destination.scheme() == "http"
                    && loopback
                    && attempt.previous().iter().all(|url| url.scheme() == "http");
                if destination.scheme() == "https" || cleartext_test {
                    attempt.follow()
                } else {
                    attempt.error("release redirects must not downgrade HTTPS")
                }
            }))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            client,
            api: base(api)?,
            downloads: base(downloads)?,
            web: base(web)?,
            cancellation: None,
            lifetime: None,
            cache: Default::default(),
            installing: Default::default(),
        })
    }

    /// Interrupt transport waits without abandoning any started blocking verification/cache work.
    pub fn with_cancellation(mut self, cancel: tokio_util::sync::CancellationToken) -> Self {
        self.cancellation = Some(cancel);
        self
    }
    /// Retain an embedding owner's lease through cache work if the calling future is dropped.
    pub fn with_lifetime_guard(mut self, guard: std::sync::Arc<dyn Send + Sync>) -> Self {
        self.lifetime = Some(OperationLifetime(guard));
        self
    }
    async fn network<T>(
        &self,
        operation: impl std::future::Future<Output = Result<T, reqwest::Error>>,
    ) -> io::Result<T> {
        if let Some(cancel) = &self.cancellation {
            tokio::select! {biased;_=cancel.cancelled()=>Err(io::Error::new(io::ErrorKind::Interrupted,"release transport cancelled during shutdown")),result=operation=>result.map_err(io::Error::other)}
        } else {
            operation.await.map_err(io::Error::other)
        }
    }
    pub async fn list(&self, include_prereleases: bool) -> io::Result<Vec<ReleaseView>> {
        // Hold the mutex through loading: callers share one successful fetch, and dropping a
        // request drops its load instead of leaving an unowned background cache-fill task.
        let mut cache = if let Some(cancel) = &self.cancellation {
            tokio::select! {biased;_=cancel.cancelled()=>return Err(io::Error::new(io::ErrorKind::Interrupted,"release transport cancelled during shutdown")),cache=self.cache.lock()=>cache}
        } else {
            self.cache.lock().await
        };
        if let Some(cached) = &*cache
            && cached.at.elapsed() < std::time::Duration::from_secs(5 * 60)
        {
            return Ok(cached
                .releases
                .iter()
                .filter(|release| include_prereleases || !release.prerelease)
                .cloned()
                .collect());
        }
        let releases = self.load_releases().await.map_err(|error| {
            io::Error::other(format!(
                "UNAVAILABLE: Could not check Gridstator releases: {error}"
            ))
        })?;
        *cache = Some(ReleaseCache {
            at: std::time::Instant::now(),
            releases,
        });
        Ok(cache
            .as_ref()
            .unwrap()
            .releases
            .iter()
            .filter(|release| include_prereleases || !release.prerelease)
            .cloned()
            .collect())
    }
    async fn load_releases(&self) -> io::Result<Vec<ReleaseView>> {
        let mut url = self.api.clone();
        url.query_pairs_mut().append_pair("per_page", "100");
        let bytes = self.read_http(url, 4 * 1024 * 1024).await?;
        let raw: Vec<PublishedRelease> = serde_json::from_slice(&bytes).map_err(invalid)?;
        let mut releases = raw
            .into_iter()
            .filter_map(|item| self.as_release(&item))
            .collect::<Vec<_>>();
        releases.sort_by_key(|release| std::cmp::Reverse(release.published_at));
        Ok(releases)
    }

    pub fn installation_guard(&self) -> io::Result<tokio::sync::MutexGuard<'_, ()>> {
        self.installing
            .try_lock()
            .map_err(|_| io::Error::other("CONFLICT: Another release is being installed."))
    }

    pub async fn fetch_manifest(&self, tag: &str) -> io::Result<VerifiedRelease> {
        validate_tag(tag)
            .map_err(|_| invalid("BAD_REQUEST: Choose a published Gridstator release tag."))?;
        let mut url = self.api.clone();
        url.path_segments_mut()
            .map_err(|_| invalid("release API cannot have path segments"))?
            .pop_if_empty()
            .push("tags")
            .push(tag);
        let bytes = self
            .read_http(url, MAX_MANIFEST_BYTES)
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "UNAVAILABLE: Could not read release {tag}: {error}"
                ))
            })?;
        let raw: PublishedRelease = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::other(format!(
                "UNAVAILABLE: Could not read release {tag}: {error}"
            ))
        })?;
        let release = self
            .as_release(&raw)
            .filter(|release| release.tag == tag)
            .ok_or_else(|| invalid(format!("NOT_FOUND: Release {tag} has no native Gridstator manifest with published checksums.")))?;
        let checksums = self
            .read_http(self.asset_url(tag, "SHA256SUMS")?, MAX_MANIFEST_BYTES)
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "UNAVAILABLE: Could not download release checksums: {error}"
                ))
            })?;
        let checksums = std::str::from_utf8(&checksums).map_err(invalid)?;
        let expected = checksum_for(checksums, RELEASE_MANIFEST_NAME).map_err(|_| {
            invalid(format!(
                "BAD_REQUEST: The published checksums do not contain {RELEASE_MANIFEST_NAME}."
            ))
        })?;
        let bytes = self
            .read_http(
                self.asset_url(tag, RELEASE_MANIFEST_NAME)?,
                MAX_MANIFEST_BYTES,
            )
            .await
            .map_err(|error| {
                io::Error::other(format!(
                    "UNAVAILABLE: Could not download release {tag}: {error}"
                ))
            })?;
        if sha256_bytes(&bytes) != expected {
            return Err(invalid(format!(
                "BAD_REQUEST: native manifest sha256 mismatch: expected {expected}, got {}",
                sha256_bytes(&bytes)
            )));
        }
        let manifest = ReleaseManifest::from_bytes(&bytes).map_err(|error| {
            invalid(format!(
                "BAD_REQUEST: Invalid native release manifest: {error}"
            ))
        })?;
        if manifest.version != release.version {
            return Err(invalid(
                "BAD_REQUEST: manifest version does not match the selected release tag",
            ));
        }
        for artifact in manifest.artifacts.values() {
            if raw
                .assets
                .iter()
                .filter(|asset| asset.name == artifact.archive.file)
                .count()
                != 1
            {
                return Err(invalid(
                    "BAD_REQUEST: release is missing a uniquely named target artifact",
                ));
            }
            if checksum_for(checksums, &artifact.archive.file)? != artifact.archive.sha256 {
                return Err(invalid(
                    "BAD_REQUEST: published checksums disagree with the native release manifest",
                ));
            }
        }
        Ok(VerifiedRelease {
            release,
            manifest,
            manifest_sha256: expected,
            manifest_bytes: bytes,
        })
    }

    pub async fn mirror_artifact(
        &self,
        release: &VerifiedRelease,
        target: &str,
        mirror: &ArtifactMirror,
    ) -> io::Result<std::path::PathBuf> {
        self.mirror_artifact_inner(release, target, mirror)
            .await
            .map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidData
                    && !error.to_string().starts_with("BAD_REQUEST:")
                {
                    invalid(format!("BAD_REQUEST: {error}"))
                } else {
                    error
                }
            })
    }
    async fn mirror_artifact_inner(
        &self,
        release: &VerifiedRelease,
        target: &str,
        mirror: &ArtifactMirror,
    ) -> io::Result<std::path::PathBuf> {
        let artifact = release.manifest.target(target)?.clone();
        let selected = release.clone();
        let destination = mirror.clone();
        let owner = self.lifetime.as_ref().map(|owner| owner.0.clone());
        blocking(move || {
            let _owner = owner;
            destination.cache_manifest(&selected.manifest_bytes, &selected.manifest_sha256)
        })
        .await?;
        let destination = mirror.clone();
        let manifest = release.manifest.clone();
        let target_owned = target.to_owned();
        if let Ok(path) =
            blocking(move || destination.verified_artifact(&manifest, &target_owned)).await
        {
            return Ok(path);
        }
        let response = self
            .network(
                self.client
                    .get(self.asset_url(&release.release.tag, &artifact.archive.file)?)
                    .send(),
            )
            .await
            .map_err(|error| release_download_error(&release.release.tag, error))?
            .error_for_status()
            .map_err(|error| {
                release_download_error(&release.release.tag, io::Error::other(error))
            })?;
        if response
            .content_length()
            .is_some_and(|size| size != artifact.archive.size)
        {
            return Err(invalid("release archive response has an unexpected length"));
        }
        let mut response = response;
        let temp = tempfile::NamedTempFile::new()?;
        let path = temp.path().to_owned();
        let mut output = tokio::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        let mut size = 0u64;
        let mut hash = Sha256::new();
        use tokio::io::AsyncWriteExt;
        while let Some(chunk) = self
            .network(response.chunk())
            .await
            .map_err(|error| release_download_error(&release.release.tag, error))?
        {
            size += chunk.len() as u64;
            if size > artifact.archive.size {
                return Err(invalid("release archive is larger than announced"));
            }
            hash.update(&chunk);
            output.write_all(&chunk).await?;
        }
        output.sync_all().await?;
        drop(output);
        let digest = hex::encode(hash.finalize());
        if size != artifact.archive.size || digest != artifact.archive.sha256 {
            return Err(invalid(format!(
                "release archive sha256 mismatch: expected {}, got {digest}",
                artifact.archive.sha256
            )));
        }
        let destination = mirror.clone();
        let manifest = release.manifest.clone();
        let target = target.to_owned();
        // Own the temporary file inside blocking work, so cancellation cannot unlink it early.
        let owner = self.lifetime.as_ref().map(|owner| owner.0.clone());
        blocking(move || {
            let _owner = owner;
            let result = destination.cache_artifact(&manifest, &target, &path);
            drop(temp);
            result
        })
        .await
    }

    fn as_release(&self, raw: &PublishedRelease) -> Option<ReleaseView> {
        let version = validate_tag(&raw.tag_name).ok()?;
        if raw.draft
            || raw
                .assets
                .iter()
                .filter(|asset| asset.name == RELEASE_MANIFEST_NAME)
                .count()
                != 1
            || raw
                .assets
                .iter()
                .filter(|asset| asset.name == "SHA256SUMS")
                .count()
                != 1
        {
            return None;
        }
        let published_at = chrono::DateTime::parse_from_rfc3339(raw.published_at.as_deref()?)
            .ok()?
            .timestamp_millis();
        let mut web = self.web.clone();
        web.path_segments_mut()
            .ok()?
            .pop_if_empty()
            .push(&raw.tag_name);
        Some(ReleaseView {
            tag: raw.tag_name.clone(),
            version: version.to_string(),
            name: raw.name.clone().unwrap_or_else(|| raw.tag_name.clone()),
            url: web.to_string(),
            published_at,
            prerelease: raw.prerelease || !version.pre.is_empty(),
            notes: raw.body.clone().unwrap_or_default(),
            manifest_name: RELEASE_MANIFEST_NAME.into(),
        })
    }

    fn asset_url(&self, tag: &str, filename: &str) -> io::Result<url::Url> {
        validate_tag(tag)?;
        validate_relative_path(filename)?;
        if Path::new(filename).components().count() != 1 {
            return Err(invalid("release asset filename must be a basename"));
        }
        let mut url = self.downloads.clone();
        url.path_segments_mut()
            .map_err(|_| invalid("download URL cannot have path segments"))?
            .pop_if_empty()
            .push(tag)
            .push(filename);
        Ok(url)
    }

    async fn read_http(&self, url: url::Url, limit: u64) -> io::Result<Vec<u8>> {
        let mut response = self
            .network(
                self.client
                    .get(url)
                    .header("accept", "application/vnd.github+json")
                    .send(),
            )
            .await?
            .error_for_status()
            .map_err(io::Error::other)?;
        if response.content_length().is_some_and(|size| size > limit) {
            return Err(invalid("release response exceeds its size limit"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = self.network(response.chunk()).await? {
            if bytes.len() as u64 + chunk.len() as u64 > limit {
                return Err(invalid("release response exceeds its size limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

#[derive(Deserialize)]
struct PublishedRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<PublishedAsset>,
}
#[derive(Deserialize)]
struct PublishedAsset {
    name: String,
}

fn validate_tag(tag: &str) -> io::Result<semver::Version> {
    if tag.len() > 128 {
        return Err(invalid("release tag is too long"));
    }
    semver::Version::parse(
        tag.strip_prefix('v')
            .ok_or_else(|| invalid("expected a published v-prefixed release tag"))?,
    )
    .map_err(invalid)
}

pub fn validate_release_tag(tag: &str) -> io::Result<()> {
    validate_tag(tag)
        .map(|_| ())
        .map_err(|_| invalid("BAD_REQUEST: Choose a published Gridstator release tag."))
}

fn release_download_error(tag: &str, error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::Interrupted {
        error
    } else {
        io::Error::other(format!(
            "UNAVAILABLE: Could not download release {tag}: {error}"
        ))
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod transport_shutdown_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn successful_release_cache_expires_after_five_minutes_and_clones_share_install_guard() {
        let source = OfficialReleases::with_base_urls(
            "http://127.0.0.1:9/",
            "http://127.0.0.1:9/",
            "http://127.0.0.1:9/",
        )
        .unwrap();
        let copy = source.clone();
        let guard = source.installation_guard().unwrap();
        assert!(
            copy.installation_guard()
                .unwrap_err()
                .to_string()
                .starts_with("CONFLICT: Another release")
        );
        drop(guard);
        assert!(copy.installation_guard().is_ok());
        *source.cache.lock().await = Some(ReleaseCache {
            at: std::time::Instant::now() - std::time::Duration::from_secs(299),
            releases: Vec::new(),
        });
        assert!(copy.list(false).await.unwrap().is_empty());
        source.cache.lock().await.as_mut().unwrap().at =
            std::time::Instant::now() - std::time::Duration::from_secs(301);
        assert!(
            copy.list(false)
                .await
                .unwrap_err()
                .to_string()
                .starts_with("UNAVAILABLE: Could not check Gridstator releases")
        );
    }
    #[tokio::test]
    async fn official_transport_cancels_stalled_headers_and_body() -> io::Result<()> {
        for body in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let (ready, started) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await?;
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await? > 0);
                if body {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\na")
                        .await?;
                }
                let _ = ready.send(());
                std::future::pending::<()>().await;
                Ok::<(), io::Error>(())
            });
            let cancel = tokio_util::sync::CancellationToken::new();
            let source = OfficialReleases::with_base_urls(
                &format!("http://{address}/"),
                &format!("http://{address}/"),
                &format!("http://{address}/"),
            )?
            .with_cancellation(cancel.clone());
            let (body_ready, body_started) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let mut response = source
                    .network(source.client.get(format!("http://{address}/")).send())
                    .await?;
                let _ = body_ready.send(());
                while source.network(response.chunk()).await?.is_some() {}
                Ok::<(), io::Error>(())
            });
            started.await.map_err(io::Error::other)?;
            if body {
                body_started.await.map_err(io::Error::other)?;
            }
            cancel.cancel();
            let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .map_err(io::Error::other)?
                .map_err(io::Error::other)?;
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
            server.abort();
        }
        Ok(())
    }
}
