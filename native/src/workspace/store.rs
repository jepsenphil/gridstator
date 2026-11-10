use super::{Capture, Manifest, ManifestEntry, digest_of, validate_digest, validate_manifest};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Verified immutable blobs. File names contain only a validated SHA-256 digest.
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn init(&self) -> Result<()> {
        private_dir(&self.root)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn path(&self, digest: &str) -> Result<PathBuf> {
        validate_digest(digest)?;
        let hash = &digest[7..];
        Ok(self.root.join("sha256").join(&hash[..2]).join(&hash[2..]))
    }
    pub fn has(&self, digest: &str) -> Result<bool> {
        let path = self.path(digest)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
    pub fn missing(&self, digests: &[String]) -> Result<Vec<String>> {
        digests
            .iter()
            .filter_map(|digest| match self.has(digest) {
                Ok(true) => None,
                Ok(false) => Some(Ok(digest.clone())),
                Err(error) => Some(Err(error)),
            })
            .collect()
    }
    pub fn open(&self, digest: &str) -> Result<File> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        Ok(options.open(self.path(digest)?)?)
    }
    pub fn read(&self, digest: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open(digest)?.read_to_end(&mut bytes)?;
        if digest_of(&bytes) != digest {
            bail!("Stored blob {digest} does not match its digest");
        }
        Ok(bytes)
    }
    /// Fully consume and verify even an already-present upload. Publish only complete blobs.
    pub fn put(
        &self,
        digest: &str,
        mut source: impl Read,
        expected_size: Option<u64>,
    ) -> Result<bool> {
        let destination = self.path(digest)?;
        let parent = destination.parent().context("invalid blob path")?;
        private_dir(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        let (actual, size) = copy_hashed(&mut source, temp.as_file_mut())?;
        if actual != digest {
            bail!("Blob digest {actual} does not match expected {digest}");
        }
        if expected_size.is_some_and(|expected| expected != size) {
            bail!("Blob size does not match expected size");
        }
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&destination) {
            Ok(_) => {
                File::open(parent)?.sync_all()?;
                Ok(true)
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Do not accept a malicious directory/symlink in place of an existing blob.
                if !self.has(digest)? {
                    bail!("Blob destination is not a regular file");
                }
                Ok(false)
            }
            Err(error) => Err(error.error.into()),
        }
    }
    pub fn put_bytes(&self, digest: &str, bytes: &[u8]) -> Result<bool> {
        self.put(digest, bytes, Some(bytes.len() as u64))
    }
    pub fn touch(&self, digest: &str) -> Result<()> {
        self.open(digest)?
            .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))?;
        Ok(())
    }
    /// Remove least-recently-used unpinned blobs until total size is at most `max_bytes`.
    /// Call only when active snapshots have been pinned, so running jobs cannot lose inputs.
    pub fn evict_lru(&self, max_bytes: u64, pinned: &BTreeSet<String>) -> Result<u64> {
        let base = self.root.join("sha256");
        let mut candidates = Vec::new();
        let mut total = 0_u64;
        if !base.exists() {
            return Ok(0);
        }
        for prefix in fs::read_dir(base)? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(prefix.path())? {
                let file = file?;
                if !file.file_type()?.is_file() {
                    continue;
                }
                let digest = format!(
                    "sha256:{}{}",
                    prefix.file_name().to_string_lossy(),
                    file.file_name().to_string_lossy()
                );
                if validate_digest(&digest).is_err() {
                    continue;
                }
                let metadata = file.metadata()?;
                total += metadata.len();
                if !pinned.contains(&digest) {
                    candidates.push((
                        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        file.path(),
                        metadata.len(),
                    ));
                }
            }
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut removed = 0;
        for (_, path, size) in candidates {
            if total <= max_bytes {
                break;
            }
            fs::remove_file(path)?;
            total = total.saturating_sub(size);
            removed += size;
        }
        Ok(removed)
    }
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn copy_hashed(source: &mut impl Read, destination: &mut impl Write) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        destination.write_all(&buffer[..count])?;
        size += count as u64;
    }
    Ok((format!("sha256:{}", hex::encode(hash.finalize())), size))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnpackLimits {
    pub max_entries: Option<usize>,
    pub max_bytes: Option<u64>,
}

/// Pack content-addressed files under `sha256/<hex>`, never under caller-controlled paths.
pub fn pack_blobs<W: Write>(store: &BlobStore, digests: &[String], output: W) -> Result<()> {
    let mut builder = tar::Builder::new(output);
    for digest in digests {
        let mut file = store.open(digest)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(file.metadata()?.len());
        header.set_mode(0o600);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        builder.append_data(&mut header, format!("sha256/{}", &digest[7..]), &mut file)?;
    }
    builder.finish()?;
    Ok(())
}

/// Write the manifest and/or selected source blobs. Sources are copied to a temporary file and
/// rehashed before sending, bounding RAM and detecting edits between capture and upload.
pub fn pack_capture_blobs<W: Write>(
    capture: &Capture,
    digests: &[String],
    output: W,
) -> Result<()> {
    let mut builder = tar::Builder::new(output);
    for digest in digests {
        validate_digest(digest)?;
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o600);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        let name = format!("sha256/{}", &digest[7..]);
        if digest == &capture.id {
            header.set_size(capture.manifest_bytes.len() as u64);
            header.set_cksum();
            builder.append_data(&mut header, name, capture.manifest_bytes.as_slice())?;
        } else {
            let source = capture
                .blobs
                .get(digest)
                .with_context(|| format!("Snapshot has no source for {digest}"))?;
            let mut temp = tempfile::tempfile()?;
            let (actual, size) = copy_hashed(&mut File::open(&source.abs_path)?, &mut temp)?;
            if &actual != digest || size != source.size {
                bail!(
                    "File {} changed after snapshot capture",
                    source.abs_path.display()
                );
            }
            use std::io::{Seek, SeekFrom};
            temp.seek(SeekFrom::Start(0))?;
            header.set_size(size);
            header.set_cksum();
            builder.append_data(&mut header, name, temp)?;
        }
    }
    builder.finish()?;
    Ok(())
}

pub fn unpack_blobs(
    source: impl Read,
    store: &BlobStore,
    limits: UnpackLimits,
) -> Result<(usize, usize, u64)> {
    let mut archive = tar::Archive::new(source);
    let (mut stored, mut present, mut bytes, mut count) = (0, 0, 0_u64, 0);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path_bytes = entry.path_bytes();
        let path = std::str::from_utf8(&path_bytes).context("Non-UTF-8 blob archive path")?;
        let Some(hex) = path.strip_prefix("sha256/") else {
            bail!("Unexpected entry {path:?} in blob archive");
        };
        let digest = format!("sha256:{hex}");
        validate_digest(&digest)
            .with_context(|| format!("Unexpected entry {path:?} in blob archive"))?;
        if !entry.header().entry_type().is_file() {
            bail!("Unexpected non-file entry in blob archive");
        }
        count += 1;
        let size = entry.header().size()?;
        bytes = bytes
            .checked_add(size)
            .context("Blob archive size overflow")?;
        if limits.max_entries.is_some_and(|max| count > max) {
            bail!("Too many blobs in one upload");
        }
        if limits.max_bytes.is_some_and(|max| bytes > max) {
            bail!("Blob upload too large");
        }
        if store.put(&digest, &mut entry, Some(size))? {
            stored += 1;
        } else {
            present += 1;
        }
    }
    Ok((stored, present, bytes))
}

fn directories(manifest: &Manifest) -> BTreeSet<&str> {
    manifest
        .entries
        .iter()
        .flat_map(|entry| {
            entry
                .path()
                .match_indices('/')
                .map(move |(index, _)| &entry.path()[..index])
        })
        .collect()
}

/// A portable workspace tar: directories first, fixed permissions and ownership, links without
/// dereferencing. The receiving executor streams this into a per-job Docker volume.
pub fn workspace_tar<W: Write>(manifest: &Manifest, store: &BlobStore, output: W) -> Result<()> {
    workspace_tar_with_owner(manifest, store, output, 0, 0)
}

/// Preserve caller-selected numeric ownership without changing portable file modes.
pub fn workspace_tar_with_owner<W: Write>(
    manifest: &Manifest,
    store: &BlobStore,
    output: W,
    uid: u64,
    gid: u64,
) -> Result<()> {
    validate_manifest(manifest)?;
    let mut builder = tar::Builder::new(output);
    for dir in directories(manifest) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_uid(uid);
        header.set_gid(gid);
        header.set_mtime(0);
        header.set_cksum();
        builder.append_data(&mut header, dir, std::io::empty())?;
    }
    for entry in &manifest.entries {
        let mut header = tar::Header::new_gnu();
        header.set_uid(uid);
        header.set_gid(gid);
        header.set_mtime(0);
        match entry {
            ManifestEntry::File(file) => {
                header.set_size(file.size);
                header.set_mode(if file.executable { 0o755 } else { 0o644 });
                header.set_cksum();
                let source = store.open(&file.digest)?;
                if source.metadata()?.len() != file.size {
                    bail!("Blob size does not match manifest for {}", file.path);
                }
                builder.append_data(&mut header, &file.path, source)?;
            }
            ManifestEntry::Symlink(link) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                builder.append_link(&mut header, &link.path, &link.target)?;
            }
        }
    }
    builder.finish()?;
    Ok(())
}

/// Materialize into a new/empty root, exclusively creating files and links. On supported Unix
/// targets every directory is opened relative to an owned directory descriptor with NOFOLLOW;
/// concurrent symlink substitution cannot redirect a write outside that root.
pub fn materialize(manifest: &Manifest, store: &BlobStore, destination: &Path) -> Result<()> {
    validate_manifest(manifest)?;
    if let Ok(metadata) = fs::symlink_metadata(destination)
        && (metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        bail!("Snapshot destination must be a real directory");
    }
    fs::create_dir_all(destination)?;
    if fs::read_dir(destination)?.next().is_some() {
        bail!(
            "Refusing to materialize snapshot into non-empty directory {}",
            destination.display()
        );
    }
    materialize_platform(manifest, store, destination)
}

#[cfg(unix)]
fn materialize_platform(manifest: &Manifest, store: &BlobStore, destination: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(destination)?;
    fn open_dir(root: &File, path: &str) -> Result<File> {
        let mut parent = root.try_clone()?;
        if path.is_empty() {
            return Ok(parent);
        }
        for segment in path.split('/') {
            let name = CString::new(segment)?;
            // SAFETY: name is NUL-terminated; parent remains owned for the call. The returned
            // descriptor is checked and transferred exactly once to File.
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            parent = unsafe { File::from_raw_fd(fd) };
        }
        Ok(parent)
    }
    fn parent_and_name(root: &File, path: &str) -> Result<(File, CString)> {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        Ok((open_dir(root, parent)?, CString::new(name)?))
    }
    for dir in directories(manifest) {
        let (parent, name) = parent_and_name(&root, dir)?;
        // SAFETY: parent is a live descriptor, name is a validated single component.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        open_dir(&root, dir)?;
    }
    for entry in &manifest.entries {
        let ManifestEntry::File(file) = entry else {
            continue;
        };
        let (parent, name) = parent_and_name(&root, &file.path)?;
        let mode = if file.executable { 0o755 } else { 0o644 };
        // SAFETY: exclusive/no-follow create cannot overwrite or traverse an existing leaf.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                // openat is variadic: Darwin mode_t is u16 and requires C integer promotion.
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut output = unsafe { File::from_raw_fd(fd) };
        let (digest, size) = copy_hashed(&mut store.open(&file.digest)?, &mut output)?;
        if digest != file.digest || size != file.size {
            bail!("Blob does not match manifest for {}", file.path);
        }
    }
    // Links come last; no file write is ever performed through a snapshot-created link.
    for entry in &manifest.entries {
        let ManifestEntry::Symlink(link) = entry else {
            continue;
        };
        let (parent, name) = parent_and_name(&root, &link.path)?;
        let target = CString::new(link.target.as_str())?;
        // SAFETY: both C strings are valid; symlinkat creates a leaf without following target.
        if unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn materialize_platform(_: &Manifest, _: &BlobStore, _: &Path) -> Result<()> {
    bail!("Safe materialization is supported on Linux and macOS targets")
}
