use gridstator::updates::{UpdateEffect, advance, in_window};
use serde_json::{Value, json};
fn node(id: &str, jobs: bool) -> Value {
    json!({"id":id,"name":id,"status":"online","info":{"build":{"id":"old","managed":true,"lastRollback":null}},"settings":{"autoUpdate":"inherit","schedulingMode":"enabled"},"activeJobs":if jobs{vec!["job"]}else{vec![]}})
}
fn persisted(nodes: &[Value], deadline: Value) -> Value {
    json!({"target":"new","failures":{},"rollout":{"id":"rollout","target":{"id":"new","version":"2"},"state":"running","now":false,"drainTimeoutSeconds":deadline,"nodes":nodes.iter().map(|node|json!({"nodeId":node["id"],"name":node["name"],"state":"pending","auto":false,"startedAt":null,"finishedAt":null,"detail":null})).collect::<Vec<_>>()}})
}
fn target() -> Value {
    json!({"id":"new","version":"2","sha256":null})
}
fn settings() -> Value {
    json!({"autoUpdate":"off","updateWindow":null})
}
#[test]
fn default_drain_never_cancels_and_progress_is_sequential() {
    let mut nodes = vec![node("a", true), node("b", false)];
    let mut state = persisted(&nodes, Value::Null);
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings(),
            &target(),
            true,
            1000,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(state["rollout"]["nodes"][0]["state"], "draining");
    assert_eq!(state["rollout"]["nodes"][1]["state"], "pending");
    assert_eq!(nodes[0]["settings"]["schedulingMode"], "draining");
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings(),
            &target(),
            true,
            24 * 3600 * 1000,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(state["rollout"]["nodes"][0]["state"], "draining");
    nodes[0]["activeJobs"] = json!([]);
    let effects = advance(
        &mut state,
        &mut nodes,
        &settings(),
        &target(),
        true,
        24 * 3600 * 1000 + 1,
        0,
    )
    .unwrap();
    assert_eq!(effects, vec![UpdateEffect::Check("a".into())]);
    assert_eq!(state["rollout"]["nodes"][1]["state"], "pending");
    nodes[0]["info"]["build"]["id"] = json!("new");
    let effects = advance(
        &mut state,
        &mut nodes,
        &settings(),
        &target(),
        true,
        24 * 3600 * 1000 + 2,
        0,
    )
    .unwrap();
    assert!(effects.contains(&UpdateEffect::Check("b".into())));
    assert_eq!(state["rollout"]["nodes"][0]["state"], "done");
    assert_eq!(nodes[0]["settings"]["schedulingMode"], "enabled");
}
#[test]
fn only_explicit_deadline_or_now_cancels_once() {
    for immediate in [false, true] {
        let mut nodes = vec![node("a", true)];
        let mut state = persisted(&nodes, json!(10));
        state["rollout"]["now"] = json!(immediate);
        let first = advance(
            &mut state,
            &mut nodes,
            &settings(),
            &target(),
            true,
            1000,
            0,
        )
        .unwrap();
        if immediate {
            assert!(
                first
                    .iter()
                    .any(|effect| matches!(effect, UpdateEffect::Cancel(..)))
            );
        } else {
            assert!(first.is_empty());
        }
        let later = advance(
            &mut state,
            &mut nodes,
            &settings(),
            &target(),
            true,
            11001,
            0,
        )
        .unwrap();
        assert_eq!(
            later
                .iter()
                .filter(|effect| matches!(effect, UpdateEffect::Cancel(..)))
                .count(),
            if immediate { 0 } else { 1 }
        );
        assert!(
            advance(
                &mut state,
                &mut nodes,
                &settings(),
                &target(),
                true,
                12000,
                0
            )
            .unwrap()
            .is_empty()
        );
    }
}
#[test]
fn rollback_halts_all_automatic_progress_and_restores_reservations() {
    let mut nodes = vec![node("a", false), node("b", false)];
    let mut state = persisted(&nodes, Value::Null);
    advance(
        &mut state,
        &mut nodes,
        &settings(),
        &target(),
        true,
        1000,
        0,
    )
    .unwrap();
    nodes[0]["info"]["build"]["lastRollback"] =
        json!({"from":"new","to":"old","at":2000,"reason":"crashed"});
    advance(
        &mut state,
        &mut nodes,
        &json!({"autoUpdate":"follow"}),
        &target(),
        true,
        2001,
        0,
    )
    .unwrap();
    assert_eq!(state["rollout"]["state"], "halted");
    assert_eq!(state["rollout"]["nodes"][0]["state"], "failed");
    assert_eq!(nodes[0]["settings"]["schedulingMode"], "enabled");
    assert_eq!(nodes[0]["update"]["state"], "failed");
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &json!({"autoUpdate":"follow"}),
            &target(),
            true,
            9000,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(state["rollout"]["nodes"][1]["state"], "pending");
}
#[test]
fn windows_wrap_midnight_and_exclude_the_end_boundary() {
    let window = json!({"start":"23:00","end":"02:00"});
    assert!(in_window(&window, 23 * 60));
    assert!(in_window(&window, 0));
    assert!(!in_window(&window, 120));
    assert!(!in_window(&json!({"start":"25:00","end":"02:00"}), 0));
    assert!(!in_window(&json!({"start":"02:00","end":"02:00"}), 120));
}
#[test]
fn auto_window_offline_update_only_and_unmanaged_skips_are_distinct() {
    let mut nodes = vec![node("a", false)];
    let mut state = json!({"target":"new","rollout":null,"failures":{}});
    let settings = json!({"autoUpdate":"window","updateWindow":{"start":"01:00","end":"02:00"}});
    advance(&mut state, &mut nodes, &settings, &target(), true, 1000, 0).unwrap();
    assert!(state["rollout"].is_null());
    assert!(
        !advance(&mut state, &mut nodes, &settings, &target(), true, 2000, 60)
            .unwrap()
            .is_empty()
    );
    let mut offline = node("offline", false);
    offline["status"] = json!("offline");
    offline["updateOnly"] = json!({"current":"old","at":5000});
    let mut nodes = vec![offline];
    let mut state = persisted(&nodes, Value::Null);
    advance(&mut state, &mut nodes, &settings, &target(), true, 5001, 0).unwrap();
    assert_eq!(state["rollout"]["nodes"][0]["state"], "updating");
    let mut nodes = vec![node("manual", false)];
    nodes[0]["info"]["build"]["managed"] = json!(false);
    let mut state = persisted(&nodes, Value::Null);
    advance(&mut state, &mut nodes, &settings, &target(), true, 5001, 0).unwrap();
    assert_eq!(state["rollout"]["nodes"][0]["state"], "skipped");
    assert_eq!(
        state["rollout"]["nodes"][0]["detail"],
        "not a managed install"
    );
    assert_eq!(nodes[0]["update"]["state"], "behind");
    assert_eq!(nodes[0]["update"]["canUpdate"], false);
    assert!(
        nodes[0]["update"]["detail"]
            .as_str()
            .unwrap()
            .contains("not a managed install")
    );
}
#[tokio::test]
async fn development_controller_reports_manual_update_reason_and_enforces_roles() {
    use axum::http::Method;
    use std::collections::HashMap;
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("controller");
    gridstator::controller::initialize(
        &directory,
        "127.0.0.1",
        7300,
        &[],
        false,
        &temporary.path().join("client.json"),
    )
    .await
    .unwrap();
    let controller = gridstator::controller::Controller::open(directory)
        .await
        .unwrap();
    let response = gridstator::updates::route(
        controller.clone(),
        Method::GET,
        "/v1/updates",
        &HashMap::new(),
        json!({}),
        json!({"kind":"client","role":"operator"}),
    )
    .await
    .unwrap()
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let view: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(view["controller"]["updatable"], false);
    assert!(view["controller"]["reason"].is_string());
    assert!(
        view["rollout"].is_null(),
        "a fresh controller has no rollout"
    );

    // Generate real reducer state, then exercise the public HTTP projection. Its shape follows
    // packages/protocol/src/update.ts RolloutSchema; reducer-only fields must not leak into nodes.
    let mut nodes = vec![node("worker", true)];
    let mut persisted = json!({"target":"new","rollout":null,"failures":{}});
    advance(
        &mut persisted,
        &mut nodes,
        &json!({"autoUpdate":"follow","updateWindow":null}),
        &target(),
        true,
        1000,
        0,
    )
    .unwrap();
    let expected = json!({
        "id":persisted["rollout"]["id"],
        "target":{"id":"new","version":"2","sha256":null},
        "state":"running","haltReason":null,"startedBy":"auto-update",
        "startedAt":1000,"finishedAt":null,"now":false,"drainTimeoutSeconds":null,
        "nodes":[{"nodeId":"worker","name":"worker","state":"draining","detail":null,"auto":true,"startedAt":1000,"finishedAt":null}]
    });
    assert!(
        persisted["rollout"]["nodes"][0]
            .get("previousMode")
            .is_some()
    );
    controller
        .db
        .write(move |connection| gridstator::db::put(connection, "updates", "rollout", &persisted))
        .await
        .unwrap();
    let response = gridstator::updates::route(
        controller.clone(),
        Method::GET,
        "/v1/updates",
        &HashMap::new(),
        json!({}),
        json!({"kind":"client","role":"operator"}),
    )
    .await
    .unwrap()
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let active: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(active["rollout"], expected);
    assert!(
        gridstator::updates::route(
            controller.clone(),
            Method::POST,
            "/v1/updates",
            &HashMap::new(),
            json!({}),
            json!({"kind":"client","role":"operator"})
        )
        .await
        .is_err()
    );
    assert!(
        gridstator::updates::route(
            controller,
            Method::GET,
            "/v2/agent/update",
            &HashMap::new(),
            json!({}),
            json!({"kind":"client","role":"admin"})
        )
        .await
        .is_err()
    );
}
fn release_fixture(root: &std::path::Path, build: &str) -> gridstator::release::ReleaseManifest {
    use gridstator::release::{
        FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest,
    };
    use std::{collections::BTreeMap, io::Write};
    let mut artifacts = BTreeMap::new();
    for target in gridstator::release::SUPPORTED_TARGETS {
        let mut executable = vec![0u8; 64];
        if target.starts_with("linux/") {
            executable[..4].copy_from_slice(b"\x7fELF");
            executable[4] = 2;
            executable[5] = 1;
            executable[18..20].copy_from_slice(
                &(if target.ends_with("amd64") {
                    62u16
                } else {
                    183u16
                })
                .to_le_bytes(),
            );
        } else {
            executable[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            executable[4..8].copy_from_slice(
                &(if target.ends_with("amd64") {
                    0x01000007u32
                } else {
                    0x0100000cu32
                })
                .to_le_bytes(),
            );
        }
        executable.extend_from_slice(build.as_bytes());
        let file = format!("gridstator-{build}-{}.tar.gz", target.replace('/', "-"));
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(executable.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, "gridstator", executable.as_slice())
            .unwrap();
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        let path = root.join(&file);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        artifacts.insert(
            target.into(),
            ReleaseArtifact {
                archive: FileDigest::from_path(file, &path).unwrap(),
                executable: FileDigest {
                    file: "gridstator".into(),
                    size: executable.len() as u64,
                    sha256: gridstator::release::sha256_bytes(&executable),
                },
            },
        );
    }
    ReleaseManifest {
        schema_version: 1,
        build_id: build.into(),
        version: "2.0.0-dev.1".into(),
        source_revision: "test fixture only; header validation is not target runtime evidence"
            .into(),
        protocol: ProtocolCompatibility::default(),
        artifacts,
    }
}
fn bundle(root: &std::path::Path, manifest: &gridstator::release::ReleaseManifest) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let bytes = manifest.to_bytes().unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, "gridstator-release.json", bytes.as_slice())
        .unwrap();
    for artifact in manifest.artifacts.values() {
        tar.append_path_with_name(root.join(&artifact.archive.file), &artifact.archive.file)
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}
#[test]
fn native_bundle_verifies_every_target_and_rejects_corrupt_or_link_members() {
    let temporary = tempfile::tempdir().unwrap();
    let manifest = release_fixture(temporary.path(), "bundle-fixture");
    let bytes = bundle(temporary.path(), &manifest);
    let path = temporary.path().join("bundle.tgz");
    std::fs::write(&path, &bytes).unwrap();
    let directory = temporary.path().join("controller");
    let imported = gridstator::updates::import_bundle(
        &path,
        &gridstator::release::sha256_bytes(&bytes),
        &directory,
    )
    .unwrap();
    assert_eq!(imported, manifest);
    let mirror = gridstator::release::ArtifactMirror::new(directory.join("native-release-mirror"));
    for target in gridstator::release::SUPPORTED_TARGETS {
        assert!(
            mirror
                .verified_artifact(&manifest, target)
                .unwrap()
                .is_file()
        );
    }
    assert!(gridstator::updates::import_bundle(&path, &"0".repeat(64), &directory).is_err());
    let archive = &manifest.artifacts["linux/amd64"].archive.file;
    std::fs::write(temporary.path().join(archive), b"corrupt").unwrap();
    let bad = bundle(temporary.path(), &manifest);
    std::fs::write(&path, &bad).unwrap();
    assert!(
        gridstator::updates::import_bundle(
            &path,
            &gridstator::release::sha256_bytes(&bad),
            &temporary.path().join("other")
        )
        .is_err()
    );
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_link_name("/etc/passwd").unwrap();
    header.set_cksum();
    tar.append_data(&mut header, "gridstator-release.json", std::io::empty())
        .unwrap();
    let bad = tar.into_inner().unwrap().finish().unwrap();
    std::fs::write(&path, &bad).unwrap();
    assert!(
        gridstator::updates::import_bundle(
            &path,
            &gridstator::release::sha256_bytes(&bad),
            &temporary.path().join("links")
        )
        .is_err()
    );
}
#[tokio::test]
async fn rollback_restores_consistent_database_and_keeps_failed_candidate_copy() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("controller");
    std::fs::create_dir_all(directory.join("updates")).unwrap();
    let db_path = directory.join("controller.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute_batch("CREATE TABLE records(kind TEXT,id TEXT,data TEXT,PRIMARY KEY(kind,id)); CREATE TABLE logs(job_id TEXT,seq INTEGER,data TEXT,PRIMARY KEY(job_id,seq));").unwrap();
    gridstator::db::put(&connection, "meta", "sentinel", &json!({"value":"old"})).unwrap();
    let snapshot = directory.join("updates/db-before-candidate.sqlite");
    connection.backup("main", &snapshot, None).unwrap();
    gridstator::db::put(
        &connection,
        "meta",
        "sentinel",
        &json!({"value":"candidate"}),
    )
    .unwrap();
    drop(connection);
    let old = release_fixture(temporary.path(), gridstator::BUILD_ID);
    let candidate = release_fixture(temporary.path(), "candidate-recovery");
    let store = gridstator::install::InstallStore::new(directory.join("install"));
    let target = gridstator::release::host_target().unwrap();
    store
        .install_verified(
            &old,
            target,
            temporary.path().join(&old.artifacts[target].archive.file),
        )
        .unwrap();
    store.activate(gridstator::BUILD_ID, None).unwrap();
    store
        .install_verified(
            &candidate,
            target,
            temporary
                .path()
                .join(&candidate.artifacts[target].archive.file),
        )
        .unwrap();
    store
        .activate(
            &candidate.build_id,
            Some(std::time::Duration::from_secs(60)),
        )
        .unwrap();
    let options = gridstator::install::SupervisorOptions {
        max_attempts: 1,
        restart_delay: std::time::Duration::from_millis(1),
        poll_interval: std::time::Duration::from_millis(5),
        forward_signals: false,
        ..Default::default()
    };
    // Header-only fixtures fail startup. Some Unix launchers report exec failure directly;
    // others return a failed child exit. Both must produce the same durable rollback record.
    if let Ok(exit) = gridstator::install::supervise(&store, options).await {
        assert_eq!(exit.build_id, gridstator::BUILD_ID);
        assert_ne!(exit.code, 0);
    }
    assert_eq!(
        store.state().unwrap().unwrap().current,
        gridstator::BUILD_ID
    );
    assert_eq!(
        store.state().unwrap().unwrap().last_rollback.unwrap().from,
        candidate.build_id
    );
    gridstator::state::write_json(&directory.join("updates/controller-switch.json"),&json!({"from":gridstator::BUILD_ID,"to":candidate.build_id,"at":gridstator::state::now()-1000,"snapshot":snapshot,"embeddedNode":null,"previousMode":null})).unwrap();
    gridstator::updates::recover_before_open(&directory).unwrap();
    let connection = rusqlite::Connection::open(db_path).unwrap();
    assert_eq!(
        gridstator::db::get(&connection, "meta", "sentinel")
            .unwrap()
            .unwrap()["value"],
        "old"
    );
    assert!(
        std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("controller.db.failed-candidate-recovery"))
    );
    assert!(!directory.join("updates/controller-switch.json").exists());
}
#[test]
fn tool_only_drift_updates_without_reinstalling_the_same_native_build() {
    let mut worker = node("a", false);
    worker["info"]["platform"] = json!("linux");
    worker["info"]["arch"] = json!("x64");
    worker["info"]["build"]["id"] = json!("new");
    worker["info"]["build"]["tools"] = json!({"act":"0.2.96","managed":["act"]});
    let mut nodes = vec![worker];
    let mut state = json!({"target":"new","rollout":null,"failures":{}});
    let mut target = target();
    target["_act"] = json!({"linux/amd64":"0.2.97"});
    let settings = json!({"autoUpdate":"follow"});
    let effects = advance(&mut state, &mut nodes, &settings, &target, true, 1000, 0).unwrap();
    assert!(effects.contains(&UpdateEffect::Check("a".into())));
    assert_eq!(nodes[0]["update"]["state"], "updating");
    nodes[0]["info"]["build"]["tools"]["act"] = json!("0.2.97");
    advance(&mut state, &mut nodes, &settings, &target, true, 2000, 0).unwrap();
    assert_eq!(state["rollout"]["state"], "finished");
    assert_eq!(nodes[0]["update"]["state"], "current");
    nodes[0]["info"]["build"]["tools"] = json!({"act":"0.0.1","managed":[],"overrides":["act"]});
    advance(&mut state, &mut nodes, &settings, &target, true, 3000, 0).unwrap();
    assert_eq!(nodes[0]["update"]["state"], "current");
}
#[test]
fn managed_act_pin_override_off_and_safe_atomic_extraction_are_distinct() {
    use gridstator::updates::act::{install_archive, wanted};
    assert_eq!(
        wanted(&json!({})).unwrap(),
        Some(("0.2.97".into(), "pinned"))
    );
    assert_eq!(wanted(&json!({"toolchain":{"act":"off"}})).unwrap(), None);
    assert_eq!(
        wanted(&json!({"toolchain":{"act":"1.2.3"}})).unwrap(),
        Some(("1.2.3".into(), "override"))
    );
    assert!(wanted(&json!({"toolchain":{"act":"../escape"}})).is_err());
    let temporary = tempfile::tempdir().unwrap();
    let release = release_fixture(temporary.path(), "act-extraction-fixture");
    let target = gridstator::release::host_target().unwrap();
    let source = temporary
        .path()
        .join(&release.artifacts[target].archive.file);
    let mut original = tar::Archive::new(flate2::read::GzDecoder::new(
        std::fs::File::open(source).unwrap(),
    ));
    let mut executable = Vec::new();
    use std::io::Read;
    original
        .entries()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .read_to_end(&mut executable)
        .unwrap();
    let archive = temporary.path().join("act.tar.gz");
    let write_archive = |link: bool| {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(executable.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "act", executable.as_slice())
            .unwrap();
        if link {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_link_name("/etc/passwd").unwrap();
            header.set_cksum();
            tar.append_data(&mut header, "other", std::io::empty())
                .unwrap();
        }
        std::fs::write(&archive, tar.into_inner().unwrap().finish().unwrap()).unwrap();
    };
    let destination = temporary.path().join("installed/act");
    write_archive(false);
    let digest = install_archive(&archive, &destination, target).unwrap();
    assert_eq!(digest, gridstator::release::sha256_bytes(&executable));
    assert_eq!(std::fs::read(&destination).unwrap(), executable);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    write_archive(true);
    assert!(install_archive(&archive, &destination, target).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), executable);
}
