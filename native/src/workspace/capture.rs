use super::{
    FileEntry, Manifest, ManifestEntry, SnapshotPatterns, SymlinkEntry, check_snapshot_path,
    digest_of, resolve_symlink_target, serialize_manifest,
};
use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CaptureOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub allow_secrets: Vec<String>,
    pub max_file_size: u64,
    pub cache_dir: Option<PathBuf>,
    pub no_git: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            allow_secrets: Vec::new(),
            max_file_size: 256 * 1024 * 1024,
            cache_dir: None,
            no_git: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureWarning {
    pub kind: String,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedFile {
    pub abs_path: PathBuf,
    pub size: u64,
}

impl CapturedFile {
    /// Recheck bytes at upload time: a successful capture does not freeze mutable source files.
    pub fn read_verified(&self, expected_digest: &str) -> Result<Vec<u8>> {
        let bytes = fs::read(&self.abs_path)?;
        if bytes.len() as u64 != self.size || digest_of(&bytes) != expected_digest {
            bail!(
                "File {} changed after snapshot capture; capture it again",
                self.abs_path.display()
            );
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStats {
    pub files: usize,
    pub symlinks: usize,
    pub total_bytes: u64,
    pub hashed_files: usize,
    pub cached_files: usize,
    pub duration_ms: u128,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub id: String,
    pub root: PathBuf,
    pub manifest: Manifest,
    #[serde(skip)]
    pub manifest_bytes: Vec<u8>,
    #[serde(skip)]
    pub blobs: BTreeMap<String, CapturedFile>,
    pub warnings: Vec<CaptureWarning>,
    pub stats: CaptureStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified_secs: i64,
    modified_nanos: i64,
    changed_secs: i64,
    changed_nanos: i64,
    inode: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    stamp: Stamp,
    digest: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HashCache {
    version: u32,
    entries: BTreeMap<String, CacheEntry>,
}

#[cfg(unix)]
fn stamp(metadata: &Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    Stamp {
        size: metadata.len(),
        modified_secs: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
        changed_secs: metadata.ctime(),
        changed_nanos: metadata.ctime_nsec(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn stamp(metadata: &Metadata) -> Stamp {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .unwrap_or_default();
    Stamp {
        size: metadata.len(),
        modified_secs: modified.as_secs() as i64,
        modified_nanos: modified.subsec_nanos() as i64,
        changed_secs: 0,
        changed_nanos: 0,
        inode: 0,
    }
}

#[cfg(unix)]
fn executable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn executable(_: &Metadata) -> bool {
    false
}

fn builtin_excluded(path: &str) -> bool {
    path.split('/')
        .any(|part| part == ".git" || part == "node_modules")
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

fn git_files(root: &Path, prefix: &str, result: &mut BTreeSet<String>) -> Result<()> {
    let output = git(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    for bytes in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
    {
        let path = std::str::from_utf8(bytes)
            .context("Git returned a non-UTF-8 filename; snapshot paths require UTF-8")?;
        // Validate before joining paths returned by external tools.
        check_snapshot_path(path)?;
        let abs = root.join(path);
        let Ok(metadata) = fs::symlink_metadata(&abs) else {
            continue;
        };
        let rel = if prefix.is_empty() {
            path.to_owned()
        } else {
            format!("{prefix}/{path}")
        };
        if metadata.is_dir() {
            if abs.join(".git").exists() {
                git_files(&abs, &rel, result)?;
            }
        } else {
            result.insert(rel);
        }
    }
    Ok(())
}

fn walk_files(
    root: &Path,
    respect_ignore: bool,
    excluded: &SnapshotPatterns,
) -> Result<Vec<String>> {
    let root_owned = root.to_owned();
    let excludes = excluded.clone();
    let matcher_error = std::sync::Arc::new(std::sync::Mutex::new(None));
    let filter_error = matcher_error.clone();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        .parents(false)
        .ignore(false)
        .git_ignore(respect_ignore)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let Ok(rel) = entry.path().strip_prefix(&root_owned) else {
                return false;
            };
            let Some(rel) = rel.to_str() else {
                return true;
            };
            if builtin_excluded(rel) {
                return false;
            }
            match excludes.is_match(rel) {
                Ok(excluded) => !excluded,
                Err(error) => {
                    *filter_error.lock().unwrap() = Some(error);
                    false
                }
            }
        });
    let mut found = Vec::new();
    for entry in builder.build() {
        let entry = entry.context("Could not walk snapshot source")?;
        if entry.depth() == 0 || entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("Snapshot paths require UTF-8 filenames")?
            .to_owned();
        found.push(rel);
    }
    if let Some(error) = matcher_error.lock().unwrap().take() {
        return Err(error);
    }
    Ok(found)
}

fn secret_reason(
    path: &str,
    abs: &Path,
    allowed: &SnapshotPatterns,
) -> Result<Option<&'static str>> {
    if allowed.is_match(path)? {
        return Ok(None);
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let exception = [
        ".env.example",
        ".env.sample",
        ".env.template",
        ".env.defaults",
    ]
    .contains(&name)
        || name.ends_with(".pub");
    let secret = name == ".env"
        || name.starts_with(".env.")
        || [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || ["id_rsa", "id_ecdsa", "id_ed25519"]
            .iter()
            .any(|key| name == *key || name.starts_with(&format!("{key}.")))
        || [".netrc", ".secrets", ".pypirc"].contains(&name)
        || path.split('/').any(|part| part == ".ssh" || part == ".aws")
        || path == ".docker/config.json"
        || path.ends_with("/.docker/config.json");
    if secret && !exception {
        return Ok(Some("matches a secret file pattern"));
    }
    if name == ".npmrc" {
        let contents = fs::read(abs)?;
        let text = String::from_utf8_lossy(&contents);
        for (index, _) in text.char_indices() {
            let rest = &text[index..];
            if index != 0
                && !text[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_whitespace() || c == ':')
            {
                continue;
            }
            for key in ["_authToken", "_auth", "_password"] {
                if rest
                    .strip_prefix(key)
                    .is_some_and(|tail| tail.trim_start().starts_with('='))
                {
                    return Ok(Some(".npmrc contains registry credentials"));
                }
            }
        }
    }
    Ok(None)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("sha256:{}", hex::encode(hash.finalize())))
}

fn warning(warnings: &mut Vec<CaptureWarning>, kind: &str, path: &str, message: String) {
    warnings.push(CaptureWarning {
        kind: kind.into(),
        path: path.into(),
        message,
    });
}

/// Capture the files currently on disk. This guarantees per-file hash consistency, not a
/// transactionally atomic tree: uploads must still verify digests against subsequent changes.
pub fn capture(root: &Path, options: impl std::borrow::Borrow<CaptureOptions>) -> Result<Capture> {
    let options = options.borrow();
    let start = Instant::now();
    let root = fs::canonicalize(root).context("Could not resolve snapshot root")?;
    if !root.is_dir() {
        bail!("Snapshot root is not a directory");
    }
    let excluded = SnapshotPatterns::new(&options.exclude)?;
    let included = SnapshotPatterns::new(&options.include)?;
    let allowed = SnapshotPatterns::new(&options.allow_secrets)?;
    let use_git = !options.no_git
        && git(&root, &["rev-parse", "--is-inside-work-tree"])
            .is_ok_and(|output| output.stdout.starts_with(b"true"));
    let mut candidates = BTreeSet::new();
    if use_git {
        git_files(&root, "", &mut candidates)?;
    } else {
        candidates.extend(walk_files(&root, true, &excluded)?);
    }
    if !options.include.is_empty() {
        for base in included.bases() {
            if !base.is_empty() && check_snapshot_path(&base).is_err() {
                continue;
            }
            let Some(directory) = include_directory(&root, &base) else {
                continue;
            };
            for path in walk_files(&directory, false, &SnapshotPatterns::default())? {
                let path = if base.is_empty() {
                    path
                } else {
                    format!("{base}/{path}")
                };
                if included.is_match(&path)? {
                    candidates.insert(path);
                }
            }
        }
    }
    let cache_path = options.cache_dir.as_ref().map(|dir| {
        let root_hash = digest_of(root.to_string_lossy().as_bytes());
        dir.join("hashcache")
            .join(format!("{}.json", &root_hash[7..39]))
    });
    let old_cache: HashCache = cache_path
        .as_ref()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|cache: &HashCache| cache.version == 1)
        .unwrap_or_default();
    let mut next_cache = HashCache {
        version: 1,
        entries: BTreeMap::new(),
    };
    let mut manifest = Manifest::default();
    let mut warnings = Vec::new();
    let mut stats = CaptureStats {
        source: if use_git { "git" } else { "walk" }.into(),
        ..Default::default()
    };
    for path in candidates {
        if let Err(error) = check_snapshot_path(&path) {
            warning(
                &mut warnings,
                "unsafe-path",
                &path,
                format!("skipped: {error}"),
            );
            continue;
        }
        if builtin_excluded(&path) || excluded.is_match(&path)? {
            continue;
        }
        let abs = root.join(&path);
        let mut captured = false;
        for _ in 0..5 {
            let before = match fs::symlink_metadata(&abs) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    captured = true;
                    break;
                }
                Err(error) => return Err(error.into()),
            };
            if before.file_type().is_symlink() {
                let target = fs::read_link(&abs)?;
                let target = target.to_str().context("Symlink target requires UTF-8")?;
                if resolve_symlink_target(&path, target).is_err() {
                    warning(
                        &mut warnings,
                        "symlink",
                        &path,
                        format!("skipped: symlink target \"{target}\" is outside the project"),
                    );
                } else {
                    manifest.entries.push(ManifestEntry::Symlink(SymlinkEntry {
                        path: path.clone(),
                        target: target.into(),
                    }));
                }
                captured = true;
                break;
            }
            if !before.is_file() {
                warning(
                    &mut warnings,
                    "unsupported",
                    &path,
                    "skipped: not a regular file".into(),
                );
                captured = true;
                break;
            }
            if let Some(reason) = secret_reason(&path, &abs, &allowed)? {
                warning(
                    &mut warnings,
                    "secret",
                    &path,
                    format!("excluded: {reason}"),
                );
                captured = true;
                break;
            }
            if before.len() > options.max_file_size {
                warning(
                    &mut warnings,
                    "too-large",
                    &path,
                    format!(
                        "excluded: {} bytes exceeds snapshot.maxFileSize",
                        before.len()
                    ),
                );
                captured = true;
                break;
            }
            let before_stamp = stamp(&before);
            let digest = if let Some(entry) = old_cache
                .entries
                .get(&path)
                .filter(|entry| entry.stamp == before_stamp)
            {
                stats.cached_files += 1;
                entry.digest.clone()
            } else {
                stats.hashed_files += 1;
                hash_file(&abs)?
            };
            let Ok(after) = fs::symlink_metadata(&abs) else {
                continue;
            };
            if stamp(&after) != before_stamp {
                continue;
            }
            let old_enough = before
                .modified()
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age.as_millis() > 2000);
            if old_enough {
                next_cache.entries.insert(
                    path.clone(),
                    CacheEntry {
                        stamp: before_stamp,
                        digest: digest.clone(),
                    },
                );
            }
            manifest.entries.push(ManifestEntry::File(FileEntry {
                path: path.clone(),
                size: before.len(),
                digest,
                executable: executable(&before),
            }));
            captured = true;
            break;
        }
        if !captured {
            bail!("File \"{path}\" kept changing while the snapshot was captured");
        }
    }
    // A tracked directory may have become a symlink; never keep descendants below a leaf.
    let leaves: HashSet<_> = manifest
        .entries
        .iter()
        .map(|e| e.path().to_owned())
        .collect();
    manifest.entries.retain(|entry| {
        !entry
            .path()
            .match_indices('/')
            .any(|(index, _)| leaves.contains(&entry.path()[..index]))
    });
    manifest
        .entries
        .sort_by(|a, b| a.path().as_bytes().cmp(b.path().as_bytes()));
    let manifest_bytes = serialize_manifest(&manifest)?;
    let id = digest_of(&manifest_bytes);
    let mut blobs = BTreeMap::new();
    for entry in &manifest.entries {
        match entry {
            ManifestEntry::File(file) => {
                stats.files += 1;
                stats.total_bytes += file.size;
                blobs
                    .entry(file.digest.clone())
                    .or_insert_with(|| CapturedFile {
                        abs_path: root.join(&file.path),
                        size: file.size,
                    });
            }
            ManifestEntry::Symlink(_) => stats.symlinks += 1,
        }
    }
    if let Some(path) = cache_path {
        // Cache persistence is only an optimization. Atomic replacement avoids torn JSON for
        // concurrent captures, and a failed cache write must not make capture fail.
        let _ = (|| -> Result<()> {
            let parent = path.parent().context("cache path has no parent")?;
            fs::create_dir_all(parent)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(temp.as_file_mut(), &next_cache)?;
            temp.persist(path).map_err(|error| error.error)?;
            Ok(())
        })();
    }
    warnings.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    stats.duration_ms = start.elapsed().as_millis();
    Ok(Capture {
        id,
        root,
        manifest,
        manifest_bytes,
        blobs,
        warnings,
        stats,
    })
}

fn include_directory(root: &Path, base: &str) -> Option<PathBuf> {
    let mut directory = root.to_owned();
    // follow_links(false) does not protect a walk's starting root. Reject symlinks in every
    // base component before constructing that root, including a symlink above its final folder.
    for component in Path::new(base).components() {
        directory.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&directory).ok()?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return None;
        }
    }
    Some(directory)
}
