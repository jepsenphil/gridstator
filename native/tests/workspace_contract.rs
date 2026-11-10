use gridstator::workspace::{
    BlobStore, CaptureOptions, FileEntry, Manifest, ManifestEntry, SnapshotPatterns, SymlinkEntry,
    UnpackLimits, capture, check_snapshot_path, digest_of, materialize, pack_blobs,
    pack_capture_blobs, parse_manifest, serialize_manifest, unpack_blobs, validate_manifest,
    workspace_tar, workspace_tar_with_owner,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn write(root: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let destination = root.join(path);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(destination, contents).unwrap();
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git is required for snapshot contract tests");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn file(path: &str, text: &str, executable: bool) -> ManifestEntry {
    ManifestEntry::File(FileEntry {
        path: path.into(),
        size: text.len() as u64,
        digest: digest_of(text),
        executable,
    })
}

fn paths(manifest: &Manifest) -> Vec<&str> {
    manifest.entries.iter().map(ManifestEntry::path).collect()
}

#[test]
fn snapshot_patterns_match_frozen_picomatch_and_source_capture_fixtures() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../contracts/workspace-globs-v1.json")).unwrap();
    let mut mismatches = Vec::new();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(fixture["rawCases"].as_array().unwrap().iter())
    {
        let patterns: Vec<String> = serde_json::from_value(case["patterns"].clone()).unwrap();
        let matcher = SnapshotPatterns::new(&patterns).unwrap();
        let expected: BTreeSet<String> = serde_json::from_value(case["matches"].clone()).unwrap();
        for path in case
            .get("paths")
            .unwrap_or(&fixture["paths"])
            .as_array()
            .unwrap()
            .iter()
            .map(|path| path.as_str().unwrap())
        {
            let actual = matcher.is_match(path).unwrap();
            if actual != expected.contains(path) {
                mismatches.push(format!("case {} patterns {patterns:?} path {path:?}: native={actual}, source={} regex {}",case["id"],expected.contains(path),case.get("regexSources").unwrap_or(&serde_json::Value::Null)));
            }
        }
        assert_eq!(
            matcher.bases(),
            serde_json::from_value::<BTreeSet<String>>(case["bases"].clone()).unwrap(),
            "scan base {}",
            case["id"]
        );
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    for case in fixture["invalid"].as_array().unwrap() {
        let patterns: Vec<String> = serde_json::from_value(case["patterns"].clone()).unwrap();
        assert_eq!(
            SnapshotPatterns::new(&patterns).err().unwrap().to_string(),
            case["error"].as_str().unwrap()
        );
    }
    for case in fixture["captureCases"].as_array().unwrap() {
        let root = TempDir::new().unwrap();
        for (path, contents) in case["files"].as_object().unwrap() {
            write(root.path(), path, contents.as_str().unwrap());
        }
        let options: CaptureOptions = serde_json::from_value(case["options"].clone()).unwrap();
        let captured = capture(root.path(), options).unwrap();
        assert_eq!(
            paths(&captured.manifest),
            serde_json::from_value::<Vec<String>>(case["paths"].clone()).unwrap(),
            "source capture {}",
            case["id"]
        );
    }
}

#[test]
fn manifest_unicode_limits_match_frozen_zod_and_explicit_path_rules() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../contracts/workspace-globs-v1.json")).unwrap();
    for case in fixture["manifestUnicodeCases"].as_array().unwrap() {
        let text = "💾".repeat(case["count"].as_u64().unwrap() as usize);
        let (path, target) = if case["field"] == "path" {
            (text, "target".into())
        } else {
            ("link".into(), text)
        };
        let manifest = Manifest {
            version: 1,
            entries: vec![ManifestEntry::Symlink(SymlinkEntry { path, target })],
        };
        assert_eq!(
            manifest.validate().is_ok(),
            case["accepted"].as_bool().unwrap(),
            "frozen source case {}",
            case["id"]
        );
    }
}

#[test]
fn snapshot_pattern_depth_and_work_are_bounded_and_capture_reports_exhaustion() {
    let nested = format!("{}a{}", "@(".repeat(128), ")".repeat(128));
    assert!(
        SnapshotPatterns::new(&[nested])
            .unwrap()
            .is_match("a")
            .unwrap()
    );
    let excessive = format!("{}a{}", "@(".repeat(129), ")".repeat(129));
    assert!(
        SnapshotPatterns::new(&[excessive])
            .err()
            .unwrap()
            .to_string()
            .contains("nesting safety limit")
    );
    assert!(SnapshotPatterns::new(&["{".repeat(65_536)]).is_err());
    let pattern = format!("{}z", "*a".repeat(500));
    let filename = "a".repeat(240);
    let matcher = SnapshotPatterns::new(std::slice::from_ref(&pattern)).unwrap();
    assert!(
        matcher
            .is_match(&filename)
            .unwrap_err()
            .to_string()
            .contains("work limit")
    );
    let root = TempDir::new().unwrap();
    write(root.path(), &filename, "content");
    let error = capture(
        root.path(),
        CaptureOptions {
            exclude: vec![pattern],
            no_git: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("work limit"), "{error:#}");
}

#[test]
fn snapshot_raw_regex_compilation_and_backtracking_are_bounded() {
    let too_many_captures = "(a)".repeat(4097);
    assert!(
        SnapshotPatterns::new(&[too_many_captures])
            .err()
            .unwrap()
            .to_string()
            .contains("capture-count safety limit")
    );
    let nested = format!("{}a{}", "(?=".repeat(129), ")".repeat(129));
    assert!(
        SnapshotPatterns::new(&[nested])
            .err()
            .unwrap()
            .to_string()
            .contains("nesting safety limit")
    );
    let pattern = SnapshotPatterns::new(&["((a+)+)+".into()]).unwrap();
    assert!(
        pattern
            .is_match(&("a".repeat(40) + "!"))
            .unwrap_err()
            .to_string()
            .contains("work limit")
    );
    let ordinary = SnapshotPatterns::new(&[
        "(?=foo)foo.ts".into(),
        "(?=bar)bar.ts".into(),
        "*.txt".into(),
    ])
    .unwrap();
    assert!(ordinary.is_match("bar.ts").unwrap());
    assert!(ordinary.is_match("ordinary.txt").unwrap());
}

#[test]
fn raw_regex_backtracking_budget_accounts_for_capture_and_program_work() {
    let input = "a".repeat(25) + "!";
    for count in [100, 500] {
        let source = format!("(({}a+)+)+", "()".repeat(count));
        let pattern = SnapshotPatterns::new(&[source]).unwrap();
        assert!(
            pattern
                .is_match(&input)
                .unwrap_err()
                .to_string()
                .contains("work limit")
        );
    }
}

#[test]
fn canonical_bytes_preserve_javascript_field_order_and_utf8_path_order() {
    let digest = digest_of("hello");
    let manifest = Manifest {
        version: 1,
        entries: vec![
            file("<&>.txt", "hello", true),
            ManifestEntry::Symlink(SymlinkEntry {
                path: "é-link".into(),
                target: "<&>.txt".into(),
            }),
            file("😀.txt", "", false),
        ],
    };
    let expected = format!(
        "{{\"version\":1,\"entries\":[{{\"path\":\"<&>.txt\",\"type\":\"file\",\"size\":5,\"digest\":\"{digest}\",\"executable\":true}},{{\"path\":\"é-link\",\"type\":\"symlink\",\"target\":\"<&>.txt\"}},{{\"path\":\"😀.txt\",\"type\":\"file\",\"size\":0,\"digest\":\"{}\",\"executable\":false}}]}}",
        digest_of("")
    );
    assert_eq!(serialize_manifest(&manifest).unwrap(), expected.as_bytes());
    assert_eq!(manifest.id().unwrap(), digest_of(expected));
    assert_eq!(
        parse_manifest(serialize_manifest(&manifest).unwrap()).unwrap(),
        manifest
    );
}

#[test]
fn manifest_rejects_traversal_duplicates_leaf_descendants_and_escaping_links() {
    for path in [
        "",
        "/absolute",
        "../escape",
        "a/../b",
        "a//b",
        "a/./b",
        "C:drive",
        "a\\b",
        "a\0b",
    ] {
        assert!(check_snapshot_path(path).is_err(), "accepted {path:?}");
    }
    for entries in [
        vec![file("a", "a", false), file("a", "b", false)],
        vec![file("b", "a", false), file("a", "b", false)],
        vec![file("a", "a", false), file("a/b", "b", false)],
        vec![ManifestEntry::Symlink(SymlinkEntry {
            path: "a".into(),
            target: "../outside".into(),
        })],
        vec![
            ManifestEntry::Symlink(SymlinkEntry {
                path: "a".into(),
                target: ".".into(),
            }),
            file("a/b", "b", false),
        ],
    ] {
        assert!(
            validate_manifest(&Manifest {
                version: 1,
                entries
            })
            .is_err()
        );
    }
    assert!(parse_manifest(br#"{"version":2,"entries":[]}"#).is_err());
    assert!(parse_manifest(br#"{"version":1,"entries":[{"path":"a","type":"file","size":1,"digest":"sha256:no","executable":false}]}"#).is_err());
}

#[test]
fn git_capture_uses_working_tree_and_applies_secret_and_include_precedence() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    git(root, &["init", "-q"]);
    write(root, ".gitignore", "dist/\n*.log\n");
    write(root, "src/index.ts", "original");
    write(root, "deleted.txt", "deleted");
    write(root, ".env", "TOKEN=tracked-secret");
    write(root, ".env.example", "TOKEN=");
    write(root, "run.sh", "#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(root, &["add", "."]);
    write(root, "src/index.ts", "uncommitted");
    fs::remove_file(root.join("deleted.txt")).unwrap();
    write(root, "src/new.ts", "new");
    write(root, "src/drop.test.ts", "test");
    write(root, ".npmrc", "//registry.example/:_authToken=private\n");
    write(
        root,
        "packages/plain/.npmrc",
        "registry=https://registry.example/\n",
    );
    write(root, "certs/private.pem", "KEY");
    write(root, ".secrets", "secret");
    write(root, "dist/output.js", "dist");
    write(root, "debug.log", "ignored");
    write(root, "node_modules/dependency.js", "excluded");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("src/index.ts", root.join("link-inside")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("link-outside")).unwrap();
    }
    let snapshot = capture(root, CaptureOptions::default()).unwrap();
    assert_eq!(snapshot.stats.source, "git");
    assert_eq!(
        paths(&snapshot.manifest),
        [
            ".env.example",
            ".gitignore",
            "link-inside",
            "packages/plain/.npmrc",
            "run.sh",
            "src/drop.test.ts",
            "src/index.ts",
            "src/new.ts"
        ]
    );
    let changed = snapshot
        .manifest
        .entries
        .iter()
        .find(|e| e.path() == "src/index.ts")
        .unwrap();
    assert_eq!(changed, &file("src/index.ts", "uncommitted", false));
    assert!(snapshot.manifest.entries.iter().any(
        |entry| matches!(entry,ManifestEntry::File(file) if file.path=="run.sh"&&file.executable)
    ));
    assert!(snapshot.manifest.entries.iter().any(|entry| matches!(entry,ManifestEntry::Symlink(link) if link.path=="link-inside"&&link.target=="src/index.ts")));
    assert_eq!(
        snapshot
            .warnings
            .iter()
            .map(|w| w.path.as_str())
            .collect::<Vec<_>>(),
        [
            ".env",
            ".npmrc",
            ".secrets",
            "certs/private.pem",
            "link-outside"
        ]
    );
    let snapshot = capture(
        root,
        CaptureOptions {
            include: vec!["dist/**".into(), "node_modules/**".into()],
            exclude: vec!["src/**/*.test.ts".into()],
            allow_secrets: vec![".env".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let selected = paths(&snapshot.manifest);
    assert!(selected.contains(&".env"));
    assert!(selected.contains(&"dist/output.js"));
    assert!(!selected.contains(&"src/drop.test.ts"));
    assert!(!selected.contains(&"debug.log"));
    assert!(!selected.iter().any(|path| path.contains("node_modules")));
}

#[test]
fn no_git_walk_obeys_nested_ignores_and_includes_hidden_files() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(root, ".gitignore", "ignored/\n");
    write(root, "a.txt", "a");
    write(root, "ignored/x.txt", "x");
    write(root, "sub/.gitignore", "*.tmp\n");
    write(root, "sub/keep.txt", "k");
    write(root, "sub/drop.tmp", "drop");
    write(root, "node_modules/m.js", "m");
    write(root, "sub/node_modules/m.js", "m");
    let snapshot = capture(root, CaptureOptions::default()).unwrap();
    assert_eq!(snapshot.stats.source, "walk");
    assert_eq!(
        paths(&snapshot.manifest),
        [".gitignore", "a.txt", "sub/.gitignore", "sub/keep.txt"]
    );
}

#[test]
fn submodules_are_recursed_using_their_uncommitted_content() {
    let parent = TempDir::new().unwrap();
    let source = TempDir::new().unwrap();
    git(source.path(), &["init", "-q"]);
    write(source.path(), "data.txt", "committed");
    git(source.path(), &["add", "."]);
    git(
        source.path(),
        &[
            "-c",
            "user.name=Contract",
            "-c",
            "user.email=contract@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    git(parent.path(), &["init", "-q"]);
    git(
        parent.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            source.path().to_str().unwrap(),
            "modules/shared",
        ],
    );
    write(parent.path(), "modules/shared/data.txt", "uncommitted");
    write(parent.path(), "modules/shared/new.txt", "untracked");
    let snapshot = capture(parent.path(), CaptureOptions::default()).unwrap();
    assert!(paths(&snapshot.manifest).contains(&"modules/shared/new.txt"));
    assert_eq!(
        snapshot
            .manifest
            .entries
            .iter()
            .find(|e| e.path() == "modules/shared/data.txt")
            .unwrap(),
        &file("modules/shared/data.txt", "uncommitted", false)
    );
    assert!(
        !paths(&snapshot.manifest)
            .iter()
            .any(|path| path.contains("/.git/"))
    );
}

#[test]
fn stat_cache_keeps_ids_stable_and_detects_same_size_rewrite() {
    let temp = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write(temp.path(), "data", "old");
    let past = SystemTime::now() - Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(temp.path().join("data"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(past))
        .unwrap();
    let options = CaptureOptions {
        cache_dir: Some(cache.path().to_owned()),
        ..Default::default()
    };
    let first = capture(temp.path(), options.clone()).unwrap();
    assert_eq!(first.id, digest_of(&first.manifest_bytes));
    let second = capture(temp.path(), options.clone()).unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.stats.cached_files, 1);
    write(temp.path(), "data", "new");
    fs::File::options()
        .write(true)
        .open(temp.path().join("data"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(past))
        .unwrap();
    assert_ne!(capture(temp.path(), options).unwrap().id, first.id);
    assert!(
        first
            .blobs
            .values()
            .next()
            .unwrap()
            .read_verified(first.blobs.keys().next().unwrap())
            .is_err()
    );
}

#[test]
fn upload_tar_verifies_blobs_deduplicates_and_materializes_exact_tree() {
    let source = TempDir::new().unwrap();
    let controller = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    write(source.path(), "src/a", "same");
    write(source.path(), "src/b", "same");
    write(source.path(), "empty", "");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            source.path().join("src/a"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("src/a", source.path().join("link")).unwrap();
    }
    let snapshot = capture(source.path(), CaptureOptions::default()).unwrap();
    assert_eq!(snapshot.blobs.len(), 2);
    let store = BlobStore::new(controller.path());
    store.init().unwrap();
    let digests: Vec<_> = std::iter::once(snapshot.id.clone())
        .chain(snapshot.blobs.keys().cloned())
        .collect();
    let mut upload = Vec::new();
    pack_capture_blobs(&snapshot, &digests, &mut upload).unwrap();
    assert_eq!(
        unpack_blobs(upload.as_slice(), &store, UnpackLimits::default()).unwrap(),
        (3, 0, snapshot.manifest_bytes.len() as u64 + 4)
    );
    assert!(store.missing(&digests).unwrap().is_empty());
    assert_eq!(
        unpack_blobs(upload.as_slice(), &store, UnpackLimits::default())
            .unwrap()
            .1,
        3
    );
    let manifest = parse_manifest(store.read(&snapshot.id).unwrap()).unwrap();
    materialize(&manifest, &store, output.path()).unwrap();
    assert_eq!(fs::read(output.path().join("src/a")).unwrap(), b"same");
    assert_eq!(fs::read(output.path().join("src/b")).unwrap(), b"same");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(output.path().join("src/a"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_eq!(
            fs::read_link(output.path().join("link")).unwrap(),
            Path::new("src/a")
        );
    }
    assert!(materialize(&manifest, &store, output.path()).is_err());
    let mut tar = Vec::new();
    workspace_tar(&manifest, &store, &mut tar).unwrap();
    let mut archive = tar::Archive::new(tar.as_slice());
    let entries: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect();
    assert_eq!(entries[0], Path::new("src"));
}

#[test]
fn store_rejects_bad_digests_unsafe_archives_and_upload_limits() {
    let temp = TempDir::new().unwrap();
    let store = BlobStore::new(temp.path());
    store.init().unwrap();
    let wrong = digest_of("wrong");
    assert!(store.put_bytes(&wrong, b"payload").is_err());
    assert!(!store.has(&wrong).unwrap());
    assert!(store.path("sha256:../../escape").is_err());
    let digest = digest_of("payload");
    for (path, kind) in [
        ("unexpected/path".to_owned(), tar::EntryType::Regular),
        (format!("sha256/{}", &digest[7..]), tar::EntryType::Symlink),
    ] {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, path, std::io::empty())
                .unwrap();
            builder.finish().unwrap();
        }
        assert!(unpack_blobs(bytes.as_slice(), &store, UnpackLimits::default()).is_err());
    }
    let source = TempDir::new().unwrap();
    write(source.path(), "payload", "payload");
    let snapshot = capture(source.path(), CaptureOptions::default()).unwrap();
    let mut bytes = Vec::new();
    pack_capture_blobs(&snapshot, &[digest], &mut bytes).unwrap();
    assert!(
        unpack_blobs(
            bytes.as_slice(),
            &store,
            UnpackLimits {
                max_entries: Some(0),
                max_bytes: None
            }
        )
        .is_err()
    );
    assert!(
        unpack_blobs(
            bytes.as_slice(),
            &store,
            UnpackLimits {
                max_entries: None,
                max_bytes: Some(6)
            }
        )
        .is_err()
    );
}

#[test]
fn cas_roundtrip_and_workspace_archive_preserve_source_acceptance() {
    let origin = TempDir::new().unwrap();
    let destination = TempDir::new().unwrap();
    let store = BlobStore::new(origin.path());
    let received = BlobStore::new(destination.path());
    store.init().unwrap();
    received.init().unwrap();
    let payloads = [b"one".to_vec(), b"two".to_vec(), vec![b'x'; 200_000]];
    let digests: Vec<_> = payloads.iter().map(digest_of).collect();
    for (bytes, digest) in payloads.iter().zip(&digests) {
        assert!(store.put_bytes(digest, bytes).unwrap());
        assert!(!store.put_bytes(digest, bytes).unwrap());
        assert_eq!(store.read(digest).unwrap(), *bytes);
    }
    assert_eq!(
        store
            .missing(&[digests[0].clone(), digest_of("absent")])
            .unwrap(),
        [digest_of("absent")]
    );
    let mut archive = Vec::new();
    pack_blobs(&store, &digests, &mut archive).unwrap();
    assert_eq!(
        unpack_blobs(archive.as_slice(), &received, UnpackLimits::default()).unwrap(),
        (3, 0, 200_006)
    );
    for (bytes, digest) in payloads.iter().zip(&digests) {
        assert_eq!(received.read(digest).unwrap(), *bytes);
    }
    let manifest = Manifest {
        version: 1,
        entries: vec![
            ManifestEntry::Symlink(SymlinkEntry {
                path: "link-inside".into(),
                target: "src/file".into(),
            }),
            ManifestEntry::File(FileEntry {
                path: "run.sh".into(),
                size: 3,
                digest: digests[0].clone(),
                executable: true,
            }),
            ManifestEntry::File(FileEntry {
                path: "src/file".into(),
                size: 3,
                digest: digests[1].clone(),
                executable: false,
            }),
        ],
    };
    let mut archive = Vec::new();
    workspace_tar_with_owner(&manifest, &store, &mut archive, 1000, 1000).unwrap();
    let mut archive = tar::Archive::new(archive.as_slice());
    let headers: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let header = entry.header();
            (
                entry.path().unwrap().to_string_lossy().into_owned(),
                header.entry_type(),
                header.mode().unwrap(),
                header.uid().unwrap(),
                header.gid().unwrap(),
                entry.link_name().unwrap().map(|p| p.into_owned()),
            )
        })
        .collect();
    for (path, kind, mode, target) in [
        ("src", tar::EntryType::Directory, 0o755, None),
        ("run.sh", tar::EntryType::Regular, 0o755, None),
        ("src/file", tar::EntryType::Regular, 0o644, None),
        (
            "link-inside",
            tar::EntryType::Symlink,
            0o777,
            Some(std::path::PathBuf::from("src/file")),
        ),
    ] {
        assert!(headers.contains(&(path.into(), kind, mode, 1000, 1000, target)));
    }
}

#[tokio::test]
async fn actual_snapshot_transport_uploads_only_missing_blobs() {
    use axum::{Json, Router, body::Bytes, routing::post};
    use gridstator::{
        client::{Client, Config},
        transfer,
    };
    use serde_json::{Value, json};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let source = TempDir::new().unwrap();
    let cas = TempDir::new().unwrap();
    write(source.path(), "src/file", "working tree bytes");
    let captured = Arc::new(capture(source.path(), CaptureOptions::default()).unwrap());
    let store = BlobStore::new(cas.path());
    store.init().unwrap();
    let uploads = Arc::new(AtomicUsize::new(0));
    let snapshots = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/v1/blobs/missing",
            post({
                let store = store.clone();
                move |Json(body): Json<Value>| {
                    let store = store.clone();
                    async move {
                        let digests: Vec<String> =
                            serde_json::from_value(body["digests"].clone()).unwrap();
                        Json(json!({"missing":store.missing(&digests).unwrap()}))
                    }
                }
            }),
        )
        .route(
            "/v1/blobs/upload",
            post({
                let store = store.clone();
                let uploads = uploads.clone();
                move |body: Bytes| {
                    let store = store.clone();
                    let uploads = uploads.clone();
                    async move {
                        uploads.fetch_add(1, Ordering::SeqCst);
                        let result = tokio::task::spawn_blocking(move || {
                            unpack_blobs(body.as_ref(), &store, UnpackLimits::default())
                        })
                        .await
                        .unwrap()
                        .unwrap();
                        Json(json!({"stored":result.0}))
                    }
                }
            }),
        )
        .route(
            "/v1/snapshots",
            post({
                let store = store.clone();
                let snapshots = snapshots.clone();
                move |Json(body): Json<Value>| {
                    let store = store.clone();
                    let snapshots = snapshots.clone();
                    async move {
                        let id = body["manifestDigest"].as_str().unwrap();
                        let manifest = parse_manifest(store.read(id).unwrap()).unwrap();
                        assert_eq!(manifest.id().unwrap(), id);
                        snapshots.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"id":id}))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = Client {
        http: reqwest::Client::new(),
        config: Config {
            version: 1,
            url: format!("http://{address}"),
            ca_pem: String::new(),
            ca_fingerprint: String::new(),
            token: "fixture".into(),
            token_name: None,
        },
    };
    let first = transfer::upload_capture_with_summary(&client, captured.clone())
        .await
        .unwrap();
    assert_eq!(first.uploaded_blobs, 2);
    assert_eq!(
        first.uploaded_bytes,
        captured.manifest_bytes.len() as u64 + b"working tree bytes".len() as u64
    );
    assert_eq!(uploads.load(Ordering::SeqCst), 1);
    let repeated = transfer::upload_capture_with_summary(&client, captured.clone())
        .await
        .unwrap();
    assert_eq!(repeated, transfer::UploadSummary::default());
    assert_eq!(
        uploads.load(Ordering::SeqCst),
        1,
        "second capture sends no blob upload request"
    );
    assert_eq!(snapshots.load(Ordering::SeqCst), 2);
    let out = TempDir::new().unwrap();
    materialize(&captured.manifest, &store, out.path()).unwrap();
    assert_eq!(
        fs::read(out.path().join("src/file")).unwrap(),
        b"working tree bytes"
    );
    server.abort();
}

#[test]
fn max_size_warnings_and_lru_pinning_are_enforced() {
    let temp = TempDir::new().unwrap();
    write(temp.path(), "big", "12345678");
    write(temp.path(), "small", "x");
    let snapshot = capture(
        temp.path(),
        CaptureOptions {
            max_file_size: 4,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(paths(&snapshot.manifest), ["small"]);
    assert_eq!(snapshot.warnings[0].kind, "too-large");
    let cache = TempDir::new().unwrap();
    let store = BlobStore::new(cache.path());
    store.init().unwrap();
    let a = digest_of("one");
    let b = digest_of("two");
    store.put_bytes(&a, b"one").unwrap();
    store.put_bytes(&b, b"two").unwrap();
    assert_eq!(store.evict_lru(3, &BTreeSet::from([a.clone()])).unwrap(), 3);
    assert!(store.has(&a).unwrap());
    assert!(!store.has(&b).unwrap());
}

#[cfg(unix)]
#[test]
fn unsafe_source_and_destination_symlinks_do_not_escape_the_root() {
    let source = TempDir::new().unwrap();
    let output = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    write(source.path(), "run.sh", "#!/bin/sh\n");
    std::os::unix::fs::symlink("../outside", source.path().join("escape")).unwrap();
    std::os::unix::fs::symlink("run.sh", source.path().join("safe")).unwrap();
    let snapshot = capture(source.path(), CaptureOptions::default()).unwrap();
    assert_eq!(paths(&snapshot.manifest), ["run.sh", "safe"]);
    assert_eq!(snapshot.warnings[0].kind, "symlink");
    let store = BlobStore::new(cache.path());
    store.init().unwrap();
    for (digest, source) in &snapshot.blobs {
        store
            .put_bytes(digest, &source.read_verified(digest).unwrap())
            .unwrap();
    }
    std::os::unix::fs::symlink(outside.path(), output.path().join("destination")).unwrap();
    assert!(
        materialize(
            &snapshot.manifest,
            &store,
            &output.path().join("destination")
        )
        .is_err()
    );
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn include_scan_bases_never_follow_symlink_components_outside_the_project() {
    let source = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write(source.path(), "ordinary.txt", "ordinary");
    write(
        outside.path(),
        "nested/password.txt",
        "must not leave this directory",
    );
    std::os::unix::fs::symlink(outside.path(), source.path().join("linked")).unwrap();
    for include in ["linked/**", "linked/nested/**"] {
        let snapshot = capture(
            source.path(),
            CaptureOptions {
                include: vec![include.into()],
                no_git: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(paths(&snapshot.manifest), ["ordinary.txt"], "{include}");
        assert!(snapshot.blobs.values().all(|blob| {
            blob.abs_path
                .starts_with(source.path().canonicalize().unwrap())
        }));
    }
}

#[test]
fn materialization_rejects_frozen_unsafe_manifests_before_writing() {
    let cas = TempDir::new().unwrap();
    let destination = TempDir::new().unwrap();
    let store = BlobStore::new(cas.path());
    store.init().unwrap();
    let digest = digest_of("x");
    store.put_bytes(&digest, b"x").unwrap();
    for entries in [
        vec![file("../escape", "x", false)],
        vec![file("/abs", "x", false)],
        vec![ManifestEntry::Symlink(SymlinkEntry {
            path: "l".into(),
            target: "../../etc".into(),
        })],
        vec![
            ManifestEntry::Symlink(SymlinkEntry {
                path: "l".into(),
                target: "x".into(),
            }),
            file("l/f", "x", false),
        ],
    ] {
        let output = destination.path().join("never-created");
        assert!(
            materialize(
                &Manifest {
                    version: 1,
                    entries
                },
                &store,
                &output
            )
            .is_err()
        );
        assert!(
            !output.exists(),
            "unsafe manifests are rejected before creating the destination"
        );
    }
}
