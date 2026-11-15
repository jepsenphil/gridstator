//! Windows snapshot staging. Directory handles deny rename/delete for the duration of the
//! operation, and opening each component without following reparse points prevents junction
//! substitution from redirecting writes outside the destination.
use super::*;
use std::collections::BTreeMap;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ,
};

fn component(name: &str) -> Result<()> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if device
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c))
    {
        bail!("Snapshot component {name:?} cannot be represented safely on Windows");
    }
    Ok(())
}

fn directory(path: &Path) -> Result<File> {
    let handle = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| format!("Open snapshot directory {}", path.display()))?;
    let metadata = handle.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!(
            "Snapshot directory must not be a reparse point: {}",
            path.display()
        );
    }
    Ok(handle)
}

struct Links<'a> {
    directories: std::collections::BTreeSet<String>,
    links: BTreeMap<String, &'a crate::workspace::SymlinkEntry>,
}
impl<'a> Links<'a> {
    fn new(manifest: &'a Manifest) -> Self {
        let directories: std::collections::BTreeSet<_> = directories(manifest)
            .into_iter()
            .map(str::to_lowercase)
            .collect();
        let links: BTreeMap<_, _> = manifest
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ManifestEntry::Symlink(link) => Some((link.path.to_lowercase(), link)),
                _ => None,
            })
            .collect();
        Self { directories, links }
    }
    fn is_directory(&self, link: &crate::workspace::SymlinkEntry) -> Result<bool> {
        let mut target = crate::workspace::resolve_symlink_target(&link.path, &link.target)?;
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert(target.to_lowercase()) || visited.len() > 64 {
                bail!(
                    "Snapshot symlink {:?} has a cyclic or excessively deep target",
                    link.path
                );
            }
            if target == "." || self.directories.contains(&target.to_lowercase()) {
                return Ok(true);
            }
            let mut redirected = None;
            let mut ends: Vec<_> = target.match_indices('/').map(|(index, _)| index).collect();
            ends.push(target.len());
            for end in ends {
                let prefix = &target[..end];
                if let Some(next) = self.links.get(&prefix.to_lowercase()) {
                    let resolved =
                        crate::workspace::resolve_symlink_target(&next.path, &next.target)?;
                    let rest = target[end..].trim_start_matches('/');
                    redirected = Some(match (resolved.as_str(), rest) {
                        (".", "") => ".".into(),
                        (".", rest) => rest.into(),
                        (_, "") => resolved,
                        _ => format!("{resolved}/{rest}"),
                    });
                    break;
                }
            }
            match redirected {
                Some(next) => target = next,
                None => return Ok(false),
            }
        }
    }
}

pub(super) fn materialize(
    manifest: &Manifest,
    store: &BlobStore,
    destination: &Path,
) -> Result<()> {
    // Validate the complete manifest before creating content. Windows paths are case insensitive
    // by default; include implicit directories so `A/file` and `a/other` cannot alias silently.
    let mut names = BTreeMap::<String, String>::new();
    for entry in &manifest.entries {
        let mut prefix = String::new();
        for part in entry.path().split('/') {
            component(part)?;
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(previous) = names.insert(prefix.to_lowercase(), prefix.clone())
                && previous != prefix
            {
                bail!("Snapshot paths {previous:?} and {prefix:?} collide on Windows");
            }
        }
    }
    let destination = std::path::absolute(destination)?;
    // Keep every ancestor pinned as well: replacing an ancestor would otherwise redirect a
    // subsequent absolute-path open even while the destination itself remains pinned.
    let mut guards = Vec::new();
    let mut ancestors: Vec<_> = destination.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        guards.push(directory(ancestor)?);
    }
    if fs::read_dir(&destination)?.next().is_some() {
        bail!("Refusing to materialize snapshot into a non-empty directory");
    }
    for dir in directories(manifest) {
        let path = destination.join(dir);
        fs::create_dir(&path)?;
        guards.push(directory(&path)?);
    }
    for entry in &manifest.entries {
        let ManifestEntry::File(file) = entry else {
            continue;
        };
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(destination.join(&file.path))?;
        let (digest, size) = copy_hashed(&mut store.open(&file.digest)?, &mut output)?;
        if digest != file.digest || size != file.size {
            bail!("Blob does not match manifest for {}", file.path);
        }
    }
    let links = Links::new(manifest);
    for entry in &manifest.entries {
        let ManifestEntry::Symlink(link) = entry else {
            continue;
        };
        // Manifest validation already checks that the target stays within the snapshot.
        let target = crate::workspace::resolve_symlink_target(&link.path, &link.target)?;
        if target != "." {
            for part in target.split('/') {
                component(part)?;
            }
        }
        let is_dir = links.is_directory(link)?;
        let result = if is_dir {
            std::os::windows::fs::symlink_dir(&link.target, destination.join(&link.path))
        } else {
            std::os::windows::fs::symlink_file(&link.target, destination.join(&link.path))
        };
        result.with_context(|| format!("Create snapshot symlink {:?}; enable Windows Developer Mode or grant symlink privileges", link.path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{FileEntry, MANIFEST_VERSION};

    #[test]
    fn rejects_windows_device_stream_and_ambiguous_names() {
        for name in [
            "CON",
            "nul.txt",
            "LPT1.log",
            "COM¹",
            "file:stream",
            "trailing.",
            "trailing ",
            "wild*",
        ] {
            assert!(component(name).is_err(), "{name}");
        }
        for name in ["console", "ordinary.txt", "COM10", "日本語"] {
            assert!(component(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn materializes_verified_bytes_and_rejects_case_aliases() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("cas"));
        let digest = super::super::digest_of(b"content");
        store.put_bytes(&digest, b"content").unwrap();
        let file = |path: &str| {
            ManifestEntry::File(FileEntry {
                path: path.into(),
                size: 7,
                digest: digest.clone(),
                executable: false,
            })
        };
        let mut manifest = Manifest {
            version: MANIFEST_VERSION,
            entries: vec![file("nested/value")],
        };
        let destination = root.path().join("out");
        super::super::materialize(&manifest, &store, &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("nested/value")).unwrap(),
            b"content"
        );
        manifest.entries = vec![file("A/one"), file("a/two")];
        assert!(
            super::super::materialize(&manifest, &store, &root.path().join("aliases")).is_err()
        );
    }

    #[test]
    fn symlink_types_follow_directory_chains_and_prefixes_and_reject_cycles() {
        use crate::workspace::SymlinkEntry;
        let link = |path: &str, target: &str| SymlinkEntry {
            path: path.into(),
            target: target.into(),
        };
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            entries: vec![
                ManifestEntry::File(FileEntry {
                    path: "actual/nested/file".into(),
                    size: 0,
                    digest: super::super::digest_of(b""),
                    executable: false,
                }),
                ManifestEntry::Symlink(link("alias", "actual")),
                ManifestEntry::Symlink(link("cycle-a", "cycle-b")),
                ManifestEntry::Symlink(link("cycle-b", "cycle-a")),
                ManifestEntry::Symlink(link("shortcut", "alias")),
            ],
        };
        manifest.validate().unwrap();
        let links = Links::new(&manifest);
        assert!(links.is_directory(&link("shortcut", "alias")).unwrap());
        assert!(links.is_directory(&link("prefix", "alias/nested")).unwrap());
        assert!(links.is_directory(&link("case", "ALIAS/NESTED")).unwrap());
        assert!(
            !links
                .is_directory(&link("file", "alias/nested/file"))
                .unwrap()
        );
        assert!(links.is_directory(&link("cycle", "cycle-a")).is_err());
    }
}
