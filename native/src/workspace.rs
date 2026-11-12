//! Working-tree snapshots with explicit canonical bytes and verified content-addressed storage.
//!
//! These operations are deliberately synchronous. Async callers should run capture, hashing and
//! archive/materialization operations on a bounded blocking worker, not on the control-plane task.
use anyhow::{Context, Result, bail};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

mod capture;
mod pattern;
mod store;
pub use capture::{Capture, CaptureOptions, CaptureStats, CaptureWarning, CapturedFile, capture};
pub use pattern::SnapshotPatterns;
pub use store::{
    BlobStore, UnpackLimits, materialize, pack_blobs, pack_capture_blobs, unpack_blobs,
    workspace_tar, workspace_tar_with_owner,
};

pub const MANIFEST_VERSION: u32 = 1;
pub const MAX_SNAPSHOT_PATH_LENGTH: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub digest: String,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymlinkEntry {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestEntry {
    File(FileEntry),
    Symlink(SymlinkEntry),
}

impl ManifestEntry {
    pub fn path(&self) -> &str {
        match self {
            Self::File(e) => &e.path,
            Self::Symlink(e) => &e.path,
        }
    }
}

// Internally-tagged enum serialization puts the discriminator first. snapshot IDs hash fixed bytes
// with `path` first, so this serialization order is a protocol decision, not derive trivia.
impl Serialize for ManifestEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::File(e) => {
                let mut out = serializer.serialize_struct("FileEntry", 5)?;
                out.serialize_field("path", &e.path)?;
                out.serialize_field("type", "file")?;
                out.serialize_field("size", &e.size)?;
                out.serialize_field("digest", &e.digest)?;
                out.serialize_field("executable", &e.executable)?;
                out.end()
            }
            Self::Symlink(e) => {
                let mut out = serializer.serialize_struct("SymlinkEntry", 3)?;
                out.serialize_field("path", &e.path)?;
                out.serialize_field("type", "symlink")?;
                out.serialize_field("target", &e.target)?;
                out.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ManifestEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type")]
        enum Wire {
            #[serde(rename = "file")]
            File {
                path: String,
                size: u64,
                digest: String,
                executable: bool,
            },
            #[serde(rename = "symlink")]
            Symlink { path: String, target: String },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::File {
                path,
                size,
                digest,
                executable,
            } => Self::File(FileEntry {
                path,
                size,
                digest,
                executable,
            }),
            Wire::Symlink { path, target } => Self::Symlink(SymlinkEntry { path, target }),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub entries: Vec<ManifestEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            entries: Vec::new(),
        }
    }
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        validate_manifest(self)
    }
    pub fn serialize(&self) -> Result<Vec<u8>> {
        serialize_manifest(self)
    }
    pub fn id(&self) -> Result<String> {
        manifest_id(self)
    }
}

pub fn digest_of(bytes: impl AsRef<[u8]>) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes.as_ref())))
}

pub fn validate_digest(digest: &str) -> Result<()> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        bail!("expected sha256:<64 lowercase hex chars>");
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        bail!("expected sha256:<64 lowercase hex chars>");
    }
    Ok(())
}

/// Validate a normalized relative path on every supported platform, independent of this host OS.
pub fn check_snapshot_path(path: &str) -> Result<()> {
    if path.is_empty() {
        bail!("empty path");
    }
    if path.encode_utf16().count() > MAX_SNAPSHOT_PATH_LENGTH {
        bail!("path too long");
    }
    if path.contains('\0') {
        bail!("path contains NUL");
    }
    if path.contains('\\') {
        bail!("path contains a backslash");
    }
    if path.starts_with('/') {
        bail!("absolute path");
    }
    if drive_path(path) {
        bail!("drive-letter path");
    }
    for part in path.split('/') {
        if part.is_empty() {
            bail!("empty path segment");
        }
        if part == "." || part == ".." {
            bail!("\"{part}\" path segment");
        }
    }
    Ok(())
}

fn drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Return the link target relative to the root, or an error when it escapes the snapshot.
pub fn resolve_symlink_target(link_path: &str, target: &str) -> Result<String> {
    if target.is_empty()
        || target.contains('\0')
        || target.contains('\\')
        || target.starts_with('/')
        || drive_path(target)
    {
        bail!("symlink target is absolute, empty, or unsafe");
    }
    // The source target schema uses Zod's code-point string limit; explicit path checks use
    // JavaScript .length and therefore remain UTF-16 above.
    if target.chars().count() > MAX_SNAPSHOT_PATH_LENGTH {
        bail!("symlink target too long");
    }
    let mut parts: Vec<&str> = link_path
        .rsplit_once('/')
        .map_or(Vec::new(), |(parent, _)| parent.split('/').collect());
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("symlink target escapes snapshot");
                }
            }
            _ => parts.push(segment),
        }
    }
    Ok(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}

pub fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.version != MANIFEST_VERSION {
        bail!("unsupported manifest version {}", manifest.version);
    }
    if manifest.entries.len() > 1_000_000 {
        bail!("too many manifest entries");
    }
    let mut previous: Option<&str> = None;
    let mut leaves = HashSet::new();
    for entry in &manifest.entries {
        let path = entry.path();
        check_snapshot_path(path)
            .map_err(|error| anyhow::anyhow!("Invalid manifest path \"{path}\": {error}"))?;
        if previous.is_some_and(|old| old.as_bytes() >= path.as_bytes()) {
            bail!("Manifest entries are not strictly sorted at \"{path}\"");
        }
        previous = Some(path);
        match entry {
            ManifestEntry::File(file) => validate_digest(&file.digest)?,
            ManifestEntry::Symlink(link) => {
                resolve_symlink_target(&link.path, &link.target).with_context(|| {
                    format!("Symlink \"{}\" points outside the snapshot", link.path)
                })?;
            }
        }
        leaves.insert(path);
    }
    for entry in &manifest.entries {
        for (index, _) in entry.path().match_indices('/') {
            let ancestor = &entry.path()[..index];
            if leaves.contains(ancestor) {
                bail!(
                    "Manifest path \"{}\" is nested under non-directory \"{ancestor}\"",
                    entry.path()
                );
            }
        }
    }
    Ok(())
}

pub fn serialize_manifest(manifest: &Manifest) -> Result<Vec<u8>> {
    validate_manifest(manifest)?;
    Ok(serde_json::to_vec(manifest)?)
}

pub fn manifest_id(manifest: &Manifest) -> Result<String> {
    Ok(digest_of(serialize_manifest(manifest)?))
}

pub fn parse_manifest(bytes: impl AsRef<[u8]>) -> Result<Manifest> {
    let manifest: Manifest =
        serde_json::from_slice(bytes.as_ref()).context("invalid snapshot manifest")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}
