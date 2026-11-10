//! Frozen update-policy assertions. Header fixtures are never executed; runtime update evidence
//! is recorded separately for actual Linux and Mac payloads.
use anyhow::{Context, Result, ensure};
use axum::{body::to_bytes, http::Method};
use gridstator::{
    controller::{self, Controller},
    db,
    release::{
        ArtifactMirror, FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest,
    },
    updates::{self, UpdateEffect, act, advance, in_window},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, collections::HashMap, path::Path, sync::Arc, time::Duration};
use tempfile::TempDir;

fn target() -> Value {
    json!({"id":"new","version":"2.0.0","sha256":null})
}
fn settings(mode: &str) -> Value {
    json!({"jobRetentionDays":90,"logRetentionDays":30,"snapshotRetentionDays":7,"autoUpdate":mode,"updateWindow":null,"toolchain":{"act":"off"}})
}
fn worker(id: &str, build: &str, managed: bool, busy: bool) -> Value {
    json!({"id":id,"name":id,"status":"online","lastSeenAt":1000,"info":{"hostname":id,"platform":"linux","arch":"x64","cpus":8,"memoryTotalMb":16384,"executors":["command"],"agentVersion":gridstator::VERSION,"build":{"id":build,"managed":managed,"runtime":"rust","target":"linux/amd64","lastRollback":null}},"settings":{"autoUpdate":"inherit","schedulingMode":"enabled","maxConcurrency":4,"reserved":{"cpus":0,"memoryMb":0}},"activeJobs":if busy{vec!["running-job"]}else{vec![]}})
}
fn empty(target: &Value) -> Value {
    json!({"target":target["id"],"rollout":null,"failures":{}})
}
fn manual(nodes: &[Value], target: &Value) -> Value {
    json!({"target":target["id"],"failures":{},"rollout":{"id":"source-rollout","target":{"id":target["id"],"version":target["version"],"sha256":target["sha256"]},"state":"running","startedBy":"operator","startedAt":1000,"finishedAt":null,"haltReason":null,"now":false,"drainTimeoutSeconds":null,"nodes":nodes.iter().map(|node|json!({"nodeId":node["id"],"name":node["name"],"state":"pending","auto":false,"startedAt":null,"finishedAt":null,"detail":null})).collect::<Vec<_>>()}})
}
fn contains(value: &Value, text: &str) {
    assert!(
        value.as_str().is_some_and(|value| value.contains(text)),
        "{value} did not contain {text:?}"
    );
}

#[test]
fn source_001_current_behind_missing_metadata_unmanaged_and_offline_status() {
    let target = target();
    let mut nodes = vec![
        worker("cur", "new", true, false),
        worker("old", "old", true, false),
        worker("legacy", "old", false, false),
        worker("unmanaged", "old", false, false),
        worker("gone", "old", false, false),
    ];
    nodes[2]["info"]["build"] = Value::Null;
    nodes[4]["info"] = Value::Null;
    nodes[4]["status"] = json!("offline");
    let mut state = empty(&target);
    advance(
        &mut state,
        &mut nodes,
        &settings("off"),
        &target,
        true,
        1000,
        0,
    )
    .unwrap();
    assert_eq!(nodes[0]["update"]["state"], "current");
    assert_eq!(nodes[0]["update"]["build"], "new");
    assert_eq!(nodes[0]["update"]["autoUpdate"], "off");
    assert_eq!(nodes[0]["update"]["canUpdate"], true);
    assert_eq!(nodes[1]["update"]["state"], "behind");
    contains(&nodes[1]["update"]["detail"], "auto-update is off");
    assert_eq!(nodes[2]["update"]["state"], "behind");
    assert_eq!(nodes[2]["update"]["canUpdate"], false);
    contains(&nodes[2]["update"]["detail"], "predates self-update");
    contains(&nodes[3]["update"]["detail"], "not a managed install");
    assert_eq!(nodes[4]["update"]["state"], "unknown");
    nodes[1]["status"] = json!("offline");
    advance(
        &mut state,
        &mut nodes,
        &settings("off"),
        &target,
        true,
        1001,
        0,
    )
    .unwrap();
    assert_eq!(nodes[1]["update"]["state"], "unknown");
    contains(&nodes[1]["update"]["detail"], "last reported old");
    assert!(
        nodes[0]["info"]["build"].get("node").is_none(),
        "native runtime metadata must not invent Node.js"
    );
}

#[test]
fn source_003_default_drain_preserves_jobs_beyond_eight_days_and_nudges_once() {
    let target = target();
    let mut nodes = vec![worker("a", "old", true, true)];
    let mut state = manual(&nodes, &target);
    assert!(state["rollout"]["drainTimeoutSeconds"].is_null());
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            1000,
            0
        )
        .unwrap()
        .is_empty()
    );
    let later = 1000 + 8 * 24 * 3_600_000;
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            later,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(state["rollout"]["nodes"][0]["state"], "draining");
    nodes[0]["activeJobs"] = json!([]);
    assert_eq!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            later + 1,
            0
        )
        .unwrap(),
        [UpdateEffect::Check("a".into())]
    );
    assert_eq!(state["rollout"]["nodes"][0]["state"], "updating");
}

#[test]
fn source_004_sequential_drain_holds_placements_and_finishes_both_returned_workers() {
    let target = target();
    let mut nodes = vec![
        worker("a", "old", true, true),
        worker("b", "old", true, false),
    ];
    let mut state = manual(&nodes, &target);
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            1000,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(state["rollout"]["nodes"][0]["state"], "draining");
    assert_eq!(state["rollout"]["nodes"][1]["state"], "pending");
    let held = json!({"id":"held","state":"queued","queuedAt":1000,"spec":{"kind":"command","resources":{"cpus":1,"memoryMb":256},"target":{"node":"a","labels":[],"prefer":[]}}});
    assert!(
        gridstator::scheduler::schedule(std::slice::from_ref(&held), &nodes, &Default::default())
            .assignments
            .is_empty()
    );
    nodes[0]["activeJobs"] = json!([]);
    assert_eq!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            1001,
            0
        )
        .unwrap(),
        [UpdateEffect::Check("a".into())]
    );
    assert_eq!(state["rollout"]["nodes"][1]["state"], "pending");
    nodes[0]["info"]["build"]["id"] = json!("new");
    assert_eq!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            1002,
            0
        )
        .unwrap(),
        [UpdateEffect::Check("b".into())]
    );
    assert_eq!(nodes[0]["settings"]["schedulingMode"], "enabled");
    assert_eq!(
        gridstator::scheduler::schedule(&[held], &nodes, &Default::default())
            .assignments
            .len(),
        1
    );
    nodes[1]["info"]["build"]["id"] = json!("new");
    advance(
        &mut state,
        &mut nodes,
        &settings("off"),
        &target,
        true,
        1003,
        0,
    )
    .unwrap();
    assert_eq!(state["rollout"]["state"], "finished");
    assert_eq!(
        state["rollout"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["done", "done"]
    );
    assert!(
        nodes
            .iter()
            .all(|node| node["update"]["state"] == "current")
    );
}

#[test]
fn source_005_now_cancels_once_then_checks_after_jobs_finish() {
    let target = target();
    let mut nodes = vec![worker("a", "old", true, true)];
    let mut state = manual(&nodes, &target);
    state["rollout"]["now"] = json!(true);
    assert_eq!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            1000,
            0
        )
        .unwrap(),
        [UpdateEffect::Cancel(
            "a".into(),
            "a is being updated".into()
        )]
    );
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            1001,
            0
        )
        .unwrap()
        .is_empty()
    );
    nodes[0]["activeJobs"] = json!([]);
    assert_eq!(
        advance(
            &mut state,
            &mut nodes,
            &settings("off"),
            &target,
            true,
            1002,
            0
        )
        .unwrap(),
        [UpdateEffect::Check("a".into())]
    );
}

#[test]
fn source_007_launcher_rollback_halts_with_source_diagnostic() {
    let target = target();
    let mut nodes = vec![worker("a", "old", true, false)];
    let mut state = manual(&nodes, &target);
    advance(
        &mut state,
        &mut nodes,
        &settings("off"),
        &target,
        true,
        1000,
        0,
    )
    .unwrap();
    nodes[0]["info"]["build"]["lastRollback"] =
        json!({"from":"new","to":"old","at":1001,"reason":"crashed"});
    advance(
        &mut state,
        &mut nodes,
        &settings("off"),
        &target,
        true,
        1002,
        0,
    )
    .unwrap();
    assert_eq!(state["rollout"]["state"], "halted");
    contains(&state["rollout"]["haltReason"], "the launcher rolled back");
    assert_eq!(nodes[0]["update"]["state"], "failed");
    assert_eq!(nodes[0]["settings"]["schedulingMode"], "enabled");
}

#[test]
fn source_008_update_deadline_halts_and_does_not_advance_the_next_worker() {
    let target = target();
    let mut nodes = vec![
        worker("a", "old", true, false),
        worker("b", "old", true, false),
    ];
    let mut state = manual(&nodes, &target);
    advance(
        &mut state,
        &mut nodes,
        &settings("off"),
        &target,
        true,
        1000,
        0,
    )
    .unwrap();
    nodes[0]["status"] = json!("offline");
    let deadline = state["rollout"]["nodes"][0]["deadline"].as_i64().unwrap();
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings("follow"),
            &target,
            true,
            deadline + 1,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(state["rollout"]["state"], "halted");
    contains(&state["rollout"]["haltReason"], "did not come back");
    assert_eq!(state["rollout"]["nodes"][1]["state"], "pending");
}

#[test]
fn source_010_follow_is_the_default_and_marks_the_rollout_automatic() {
    let target = target();
    let mut nodes = vec![worker("a", "old", true, false)];
    let mut state = empty(&target);
    assert_eq!(
        advance(&mut state, &mut nodes, &json!({}), &target, true, 1000, 0).unwrap(),
        [UpdateEffect::Check("a".into())]
    );
    assert_eq!(state["rollout"]["startedBy"], "auto-update");
    assert_eq!(nodes[0]["update"]["autoUpdate"], "follow");
}

#[test]
fn source_011_node_override_bypasses_a_closed_window() {
    let target = target();
    let mut nodes = vec![worker("a", "old", true, false)];
    let mut state = empty(&target);
    let mut settings = settings("window");
    settings["updateWindow"] = json!({"start":"02:00","end":"03:00"});
    assert!(
        advance(&mut state, &mut nodes, &settings, &target, true, 1000, 0)
            .unwrap()
            .is_empty()
    );
    contains(
        &nodes[0]["update"]["detail"],
        "waiting for the update window",
    );
    nodes[0]["settings"]["autoUpdate"] = json!("follow");
    assert_eq!(
        advance(&mut state, &mut nodes, &settings, &target, true, 1001, 0).unwrap(),
        [UpdateEffect::Check("a".into())]
    );
}

#[test]
fn source_013_normal_and_wrapping_window_boundaries_match_source() {
    let normal = json!({"start":"02:00","end":"05:00"});
    assert!(in_window(&normal, 3 * 60));
    assert!(!in_window(&normal, 5 * 60));
    let wrapping = json!({"start":"22:00","end":"02:00"});
    assert!(in_window(&wrapping, 23 * 60 + 30));
    assert!(in_window(&wrapping, 60 + 59));
    assert!(!in_window(&wrapping, 12 * 60));
}

#[test]
fn source_016_unmanaged_same_build_ignores_managed_act_drift() {
    let mut target = target();
    target["_act"] = json!({"linux/amd64":act::PIN});
    let mut nodes = vec![worker("a", "new", false, false)];
    nodes[0]["info"]["build"]["tools"] = json!({"act":"0.2.80","managed":[]});
    let mut state = empty(&target);
    assert!(
        advance(
            &mut state,
            &mut nodes,
            &settings("follow"),
            &target,
            true,
            1000,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(nodes[0]["update"]["state"], "current");
    assert_eq!(nodes[0]["update"]["canUpdate"], false);
    assert!(state["rollout"].is_null());
}

fn executable(target: &str) -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    if target.starts_with("linux/") {
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(
            &(if target.ends_with("amd64") {
                62u16
            } else {
                183u16
            })
            .to_le_bytes(),
        );
    } else {
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[4..8].copy_from_slice(
            &(if target.ends_with("amd64") {
                0x01000007u32
            } else {
                0x0100000cu32
            })
            .to_le_bytes(),
        );
    }
    bytes
}
fn archive(name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o755);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)?;
    Ok(tar.into_inner()?.finish()?)
}
fn mirror_fixture(directory: &Path) -> Result<ReleaseManifest> {
    let mirror = ArtifactMirror::new(directory.join("native-release-mirror"));
    let mut artifacts = BTreeMap::new();
    for target in gridstator::release::SUPPORTED_TARGETS {
        let bytes = executable(target);
        let name = format!("gridstator-{}.tar.gz", target.replace('/', "-"));
        let path = directory.join(&name);
        std::fs::write(&path, archive("gridstator", &bytes)?)?;
        artifacts.insert(
            target.into(),
            ReleaseArtifact {
                archive: FileDigest::from_path(name, &path)?,
                executable: FileDigest {
                    file: "gridstator".into(),
                    sha256: gridstator::release::sha256_bytes(&bytes),
                    size: bytes.len() as u64,
                },
            },
        );
    }
    let manifest = ReleaseManifest {
        schema_version: 1,
        build_id: gridstator::BUILD_ID.into(),
        version: gridstator::VERSION.into(),
        source_revision: "deterministic header fixture; not executable runtime evidence".into(),
        protocol: ProtocolCompatibility::default(),
        artifacts,
    };
    mirror.cache_manifest(&manifest.to_bytes()?, &manifest.sha256()?)?;
    for (target, artifact) in &manifest.artifacts {
        mirror.cache_artifact(&manifest, target, directory.join(&artifact.archive.file))?;
    }
    Ok(manifest)
}
struct Fixture {
    _temporary: TempDir,
    controller: Arc<Controller>,
    manifest: ReleaseManifest,
}
impl Fixture {
    async fn new(nodes: Vec<Value>, settings: Value) -> Result<Self> {
        let temporary = TempDir::new()?;
        let directory = temporary.path().join("controller");
        controller::initialize(
            &directory,
            "127.0.0.1",
            0,
            &[],
            false,
            &temporary.path().join("client.json"),
        )
        .await?;
        let manifest = mirror_fixture(&directory)?;
        let controller = Controller::open(directory).await?;
        controller
            .db
            .write(move |connection| {
                db::put(connection, "settings", "cluster", &settings)?;
                for node in nodes {
                    db::put(connection, "nodes", node["id"].as_str().unwrap(), &node)?;
                }
                Ok(())
            })
            .await?;
        Ok(Self {
            _temporary: temporary,
            controller,
            manifest,
        })
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        query: HashMap<String, String>,
        input: Value,
        principal: Value,
    ) -> Result<Value> {
        let response = updates::route(
            self.controller.clone(),
            method,
            path,
            &query,
            input,
            principal,
        )
        .await?
        .context("route not found")?;
        Ok(serde_json::from_slice(
            &to_bytes(response.into_body(), 8 * 1024 * 1024).await?,
        )?)
    }
    async fn view(&self) -> Result<Value> {
        self.request(
            Method::GET,
            "/v1/updates",
            HashMap::new(),
            json!({}),
            admin(),
        )
        .await
    }
    async fn nodes(&self) -> Result<Vec<Value>> {
        self.controller
            .db
            .call(|connection| db::list(connection, "nodes"))
            .await
    }
    async fn set_nodes(&self, nodes: Vec<Value>) -> Result<()> {
        self.controller
            .db
            .write(move |connection| {
                for node in nodes {
                    db::put(connection, "nodes", node["id"].as_str().unwrap(), &node)?;
                }
                Ok(())
            })
            .await
    }
}
fn admin() -> Value {
    json!({"kind":"client","role":"admin","name":"source-operator"})
}
fn agent(id: &str) -> Value {
    json!({"kind":"agent","nodeId":id})
}

#[tokio::test]
async fn source_002_native_unbundled_replacement_rejects_admin_update() -> Result<()> {
    let fixture = Fixture::new(vec![], settings("off")).await?;
    std::fs::remove_dir_all(fixture.controller.dir.join("native-release-mirror"))?;
    let view = fixture.view().await?;
    assert_eq!(view["controller"]["updatable"], false);
    assert_eq!(view["controller"]["runtime"], "rust");
    contains(&view["controller"]["reason"], "verified");
    let error = fixture
        .request(
            Method::POST,
            "/v1/updates",
            HashMap::new(),
            json!({}),
            admin(),
        )
        .await
        .unwrap_err();
    contains(&json!(error.to_string()), "cannot update");
    Ok(())
}

#[tokio::test]
async fn source_006_report_halts_with_node_prefix_projects_failure_and_allows_explicit_retry()
-> Result<()> {
    let fixture = Fixture::new(
        vec![
            worker("a", "old", true, false),
            worker("b", "old", true, false),
        ],
        settings("off"),
    )
    .await?;
    let started = fixture
        .request(
            Method::POST,
            "/v1/updates",
            HashMap::new(),
            json!({"nodes":["a","b"]}),
            admin(),
        )
        .await?;
    assert!(started["rollout"]["drainTimeoutSeconds"].is_null());
    fixture
        .request(
            Method::POST,
            "/v2/agent/update/report",
            HashMap::new(),
            json!({"target":gridstator::BUILD_ID,"error":"disk full"}),
            agent("a"),
        )
        .await?;
    let view = fixture.view().await?;
    assert_eq!(view["rollout"]["state"], "halted");
    assert_eq!(view["rollout"]["haltReason"], "a: disk full");
    assert_eq!(
        view["rollout"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (&entry["name"], &entry["state"]))
            .collect::<Vec<_>>(),
        vec![
            (&json!("a"), &json!("failed")),
            (&json!("b"), &json!("pending"))
        ]
    );
    let nodes = fixture.nodes().await?;
    assert_eq!(nodes[0]["update"]["state"], "failed");
    assert_eq!(nodes[0]["update"]["detail"], "disk full");
    contains(&nodes[1]["update"]["detail"], "rollout halted");
    fixture
        .controller
        .db
        .write(|connection| db::put(connection, "settings", "cluster", &settings("follow")))
        .await?;
    updates::tick(fixture.controller.clone()).await?;
    assert_eq!(
        fixture.view().await?["rollout"]["nodes"][1]["state"],
        "pending"
    );
    let retried = fixture
        .request(
            Method::POST,
            "/v1/updates",
            HashMap::new(),
            json!({"nodes":["a"]}),
            admin(),
        )
        .await?;
    assert_eq!(retried["rollout"]["state"], "running");
    assert_eq!(retried["rollout"]["nodes"][0]["state"], "updating");
    Ok(())
}

#[tokio::test]
async fn source_006_a_report_from_a_pending_worker_does_not_halt_the_active_worker() -> Result<()> {
    let fixture = Fixture::new(
        vec![
            worker("a", "old", true, false),
            worker("b", "old", true, false),
        ],
        settings("off"),
    )
    .await?;
    fixture
        .request(
            Method::POST,
            "/v1/updates",
            HashMap::new(),
            json!({"nodes":["a","b"]}),
            admin(),
        )
        .await?;
    fixture
        .request(
            Method::POST,
            "/v2/agent/update/report",
            HashMap::new(),
            json!({"target":gridstator::BUILD_ID,"error":"disk full"}),
            agent("b"),
        )
        .await?;
    let view = fixture.view().await?;
    assert_eq!(view["rollout"]["state"], "running");
    assert_eq!(view["rollout"]["nodes"][0]["state"], "updating");
    assert_eq!(view["rollout"]["nodes"][1]["state"], "pending");
    assert_eq!(fixture.nodes().await?[1]["update"]["state"], "failed");
    Ok(())
}

#[tokio::test]
async fn source_004_native_offers_wait_for_drain_and_only_one_worker_gets_a_candidate() -> Result<()>
{
    let fixture = Fixture::new(
        vec![
            worker("a", "old", true, true),
            worker("b", "old", true, false),
        ],
        settings("off"),
    )
    .await?;
    let started = fixture
        .request(
            Method::POST,
            "/v1/updates",
            HashMap::new(),
            json!({"nodes":["a","b"],"now":false,"drainTimeoutSeconds":null}),
            admin(),
        )
        .await?;
    assert_eq!(started["controller"]["id"], gridstator::BUILD_ID);
    assert_eq!(started["controller"]["updatable"], true);
    assert!(started["controller"]["reason"].is_null());
    assert!(started["rollout"]["drainTimeoutSeconds"].is_null());
    let query = HashMap::from([
        ("mode".into(), "normal".into()),
        ("current".into(), "old".into()),
        ("target".into(), gridstator::release::host_target()?.into()),
    ]);
    for id in ["a", "b"] {
        assert_eq!(
            fixture
                .request(
                    Method::GET,
                    "/v2/agent/update",
                    query.clone(),
                    json!({}),
                    agent(id)
                )
                .await?["action"],
            "none"
        );
    }
    let mut nodes = fixture.nodes().await?;
    nodes[0]["activeJobs"] = json!([]);
    fixture.set_nodes(nodes).await?;
    updates::tick(fixture.controller.clone()).await?;
    let offered = fixture
        .request(
            Method::GET,
            "/v2/agent/update",
            query.clone(),
            json!({}),
            agent("a"),
        )
        .await?;
    assert_eq!(offered["action"], "update");
    assert_eq!(offered["buildId"], gridstator::BUILD_ID);
    assert_eq!(offered["confirmWithinSeconds"], 600);
    assert_eq!(
        fixture
            .request(
                Method::GET,
                "/v2/agent/update",
                query.clone(),
                json!({}),
                agent("b")
            )
            .await?["action"],
        "none"
    );
    let mut nodes = fixture.nodes().await?;
    nodes[0]["info"]["build"]["id"] = json!(gridstator::BUILD_ID);
    fixture.set_nodes(nodes).await?;
    updates::tick(fixture.controller.clone()).await?;
    assert_eq!(
        fixture.nodes().await?[0]["settings"]["schedulingMode"],
        "enabled"
    );
    assert_eq!(
        fixture
            .request(
                Method::GET,
                "/v2/agent/update",
                query,
                json!({}),
                agent("b")
            )
            .await?["action"],
        "update"
    );
    let mut nodes = fixture.nodes().await?;
    nodes[1]["info"]["build"]["id"] = json!(gridstator::BUILD_ID);
    fixture.set_nodes(nodes).await?;
    updates::tick(fixture.controller.clone()).await?;
    let done = fixture.view().await?;
    assert_eq!(done["rollout"]["state"], "finished");
    assert!(
        done["rollout"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["state"] == "done")
    );
    Ok(())
}

#[tokio::test]
async fn source_009_cancel_rollout_restores_worker_and_projects_behind_immediately() -> Result<()> {
    let fixture = Fixture::new(vec![worker("a", "old", true, true)], settings("off")).await?;
    fixture
        .request(
            Method::POST,
            "/v1/updates",
            HashMap::new(),
            json!({"nodes":["a"]}),
            admin(),
        )
        .await?;
    assert_eq!(
        fixture.nodes().await?[0]["settings"]["schedulingMode"],
        "draining"
    );
    let cancelled = fixture
        .request(
            Method::POST,
            "/v1/updates/cancel",
            HashMap::new(),
            json!({}),
            admin(),
        )
        .await?;
    assert_eq!(cancelled["rollout"]["state"], "halted");
    contains(&cancelled["rollout"]["haltReason"], "stopped by");
    let nodes = fixture.nodes().await?;
    assert_eq!(nodes[0]["settings"]["schedulingMode"], "enabled");
    assert_eq!(nodes[0]["update"]["state"], "behind");
    Ok(())
}

#[tokio::test]
async fn source_012_update_only_bootstraps_each_native_target_without_agent_socket() -> Result<()> {
    for target in gridstator::release::SUPPORTED_TARGETS {
        let mut node = worker("a", "old", false, false);
        node["status"] = json!("offline");
        let fixture = Fixture::new(vec![node], settings("follow")).await?;
        let query = HashMap::from([
            ("mode".into(), "update-only".into()),
            ("current".into(), "old".into()),
            ("target".into(), target.into()),
        ]);
        let offered = fixture
            .request(
                Method::GET,
                "/v2/agent/update",
                query,
                json!({}),
                agent("a"),
            )
            .await?;
        assert_eq!(offered["action"], "update");
        assert_eq!(offered["buildId"], gridstator::BUILD_ID);
        assert_eq!(offered["manifestSha256"], fixture.manifest.sha256()?);
        assert_eq!(
            offered["artifact"]["sha256"],
            fixture.manifest.artifacts[target].archive.sha256
        );
        assert_eq!(offered["confirmWithinSeconds"], 600);
        assert!(offered.get("entry").is_none() && offered.get("node").is_none());
        assert_eq!(fixture.nodes().await?[0]["update"]["state"], "updating");
        let response = updates::route(
            fixture.controller.clone(),
            Method::GET,
            "/v2/agent/update/artifact",
            &HashMap::from([
                ("buildId".into(), gridstator::BUILD_ID.into()),
                ("target".into(), target.into()),
            ]),
            json!({}),
            agent("a"),
        )
        .await?
        .unwrap();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
        assert_eq!(
            gridstator::release::sha256_bytes(&bytes),
            fixture.manifest.artifacts[target].archive.sha256
        );
        let mut nodes = fixture.nodes().await?;
        nodes[0]["status"] = json!("online");
        nodes[0]["info"]["build"]["id"] = json!(gridstator::BUILD_ID);
        nodes[0]["info"]["build"]["managed"] = json!(true);
        fixture.set_nodes(nodes).await?;
        updates::tick(fixture.controller.clone()).await?;
        assert_eq!(fixture.nodes().await?[0]["update"]["state"], "current");
        assert_eq!(fixture.view().await?["rollout"]["state"], "finished");
    }
    Ok(())
}

#[tokio::test]
async fn source_011_api_rejects_window_mode_without_a_window() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temporary = TempDir::new()?;
    let directory = temporary.path().join("controller");
    let config = temporary.path().join("client.json");
    controller::initialize(&directory, "127.0.0.1", 0, &[], false, &config).await?;
    struct Stop(tokio::task::JoinHandle<Result<()>>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Stop(tokio::spawn(controller::start(
        directory.clone(),
        None,
        None,
    )));
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(runtime) = gridstator::state::read_json(&directory.join("runtime.json")) {
                let mut config: gridstator::client::Config =
                    serde_json::from_slice(&std::fs::read(&config)?)?;
                config.url = format!("https://127.0.0.1:{}", runtime["port"]);
                let client = gridstator::client::Client::new(config)?;
                if client.get("/v1/health").await.is_ok() {
                    break Ok::<_, anyhow::Error>(client);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let error = client
        .request(
            Method::PATCH,
            "/v1/settings",
            Some(json!({"autoUpdate":"window","updateWindow":null})),
        )
        .await
        .expect_err("window mode without a window must be rejected");
    contains(&json!(error.to_string()), "needs an update window");
    Ok(())
}

#[tokio::test]
async fn source_005_actual_api_now_cancels_the_job_before_nudging_its_worker() -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temporary = TempDir::new()?;
    let directory = temporary.path().join("controller");
    let config = temporary.path().join("client.json");
    controller::initialize(&directory, "127.0.0.1", 0, &[], false, &config).await?;
    mirror_fixture(&directory)?;
    db::Db::open(&directory.join("controller.db"))?
        .write(|connection| db::put(connection, "settings", "cluster", &settings("off")))
        .await?;
    struct Stop(tokio::task::JoinHandle<Result<()>>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Stop(tokio::spawn(controller::start(
        directory.clone(),
        None,
        None,
    )));
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(runtime) = gridstator::state::read_json(&directory.join("runtime.json")) {
                let mut config: gridstator::client::Config =
                    serde_json::from_slice(&std::fs::read(&config)?)?;
                config.url = format!("https://127.0.0.1:{}", runtime["port"]);
                let client = gridstator::client::Client::new(config)?;
                if client.get("/v1/health").await.is_ok() {
                    break Ok::<_, anyhow::Error>(client);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let join = client
        .post("/v1/tokens", json!({"kind":"join","name":"source-now"}))
        .await?;
    let mut pairing = client.config.clone();
    pairing.token = join["token"].as_str().unwrap().into();
    let info = json!({"hostname":"a","platform":"linux","arch":"x64","osRelease":"test","cpus":8,"memoryTotalMb":16384,"agentVersion":gridstator::VERSION,"docker":{"available":true},"buildx":{"available":true},"act":{"available":false},"executors":["command"],"build":{"id":"old","version":"2.0.0","managed":true,"lastRollback":null,"runtime":"rust","target":"linux/amd64"}});
    let registered = gridstator::client::Client::new(pairing)?
        .post("/v1/agents/register", json!({"name":"a","info":info}))
        .await?;
    let mut request = format!(
        "{}/v1/agent/ws",
        client.config.url.replacen("https:", "wss:", 1)
    )
    .into_client_request()?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", registered["credential"].as_str().unwrap()).parse()?,
    );
    let (mut socket, _) = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(
            gridstator::pki::websocket_tls(&client.config.ca_pem)?,
        )),
    )
    .await?;
    async fn next(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        kind: &str,
    ) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let message = socket.next().await.context("agent closed")??;
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(&text)?;
                    if value["type"] == kind {
                        return Ok::<_, anyhow::Error>(value);
                    }
                }
            }
        })
        .await?
    }
    socket
        .send(Message::Text(
            json!({"type":"hello","protocolVersion":1,"info":info,"activeJobs":[],"results":[]})
                .to_string()
                .into(),
        ))
        .await?;
    next(&mut socket, "welcome").await?;
    let job = client.post("/v1/jobs",json!({"spec":{"kind":"command","image":"alpine","command":["sleep","100"],"resources":{"cpus":1,"memoryMb":256},"target":{"node":"a"}}})).await?;
    let assignment = next(&mut socket, "job.assign").await?;
    assert_eq!(assignment["jobId"], job["id"]);
    socket.send(Message::Text(json!({"type":"job.state","jobId":job["id"],"assignmentId":assignment["assignmentId"],"phase":"running","ts":gridstator::state::now()}).to_string().into())).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if client
                .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
                .await?["state"]
                == "running"
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    client
        .post("/v1/updates", json!({"nodes":["a"],"now":true}))
        .await?;
    assert_eq!(next(&mut socket, "job.cancel").await?["jobId"], job["id"]);
    socket.send(Message::Text(json!({"type":"job.result","result":{"jobId":job["id"],"assignmentId":assignment["assignmentId"],"outcome":"cancelled","exitCode":null,"error":null,"oomKilled":false,"finishedAt":gridstator::state::now(),"logSeq":0,"outputs":null,"artifacts":[]}}).to_string().into())).await?;
    next(&mut socket, "update.check").await?;
    assert_eq!(
        client
            .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
            .await?["state"],
        "cancelled"
    );
    Ok(())
}

async fn seed_act(fixture: &Fixture, version: &str) -> Result<()> {
    let mut records = Vec::new();
    for target in gridstator::release::SUPPORTED_TARGETS {
        let bytes = archive("act", &executable(target))?;
        let path = fixture
            .controller
            .dir
            .join(format!("act-{version}-{}.tar.gz", target.replace('/', "-")));
        std::fs::write(&path, &bytes)?;
        records.push((format!("act-{version}-{}",target.replace('/',"-")),json!({"state":"ready","path":path,"sha256":gridstator::release::sha256_bytes(&bytes),"size":bytes.len(),"reason":null})));
    }
    fixture
        .controller
        .db
        .write(move |connection| {
            for (key, value) in records {
                db::put(connection, "toolchain", &key, &value)?;
            }
            Ok(())
        })
        .await
}

#[tokio::test]
async fn source_014_native_act_only_drift_verified_archives_and_four_target_offers() -> Result<()> {
    let mut node = worker("a", gridstator::BUILD_ID, true, false);
    node["info"]["build"]["tools"] = json!({"act":"0.2.80","managed":[]});
    let mut settings = settings("off");
    settings["toolchain"]["act"] = Value::Null;
    let fixture = Fixture::new(vec![node], settings).await?;
    seed_act(&fixture, act::PIN).await?;
    updates::tick(fixture.controller.clone()).await?;
    let node = &fixture.nodes().await?[0];
    assert_eq!(node["update"]["state"], "behind");
    contains(&node["update"]["detail"], "toolchain");
    contains(
        &node["update"]["detail"],
        &format!("act 0.2.80 (system) → {}", act::PIN),
    );
    let view = fixture.view().await?;
    assert_eq!(view["toolchain"].as_array().unwrap().len(), 1);
    assert_eq!(view["toolchain"][0]["name"], "act");
    assert_eq!(view["toolchain"][0]["version"], act::PIN);
    assert_eq!(view["toolchain"][0]["source"], "pinned");
    for target in gridstator::release::SUPPORTED_TARGETS {
        let offered = act::offer(&fixture.controller, target).await?;
        let tool = &offered["tools"][0];
        assert_eq!(tool["state"], "ready");
        assert_eq!(tool["bin"], "act");
        assert_eq!(tool["target"], target);
        let response = updates::route(
            fixture.controller.clone(),
            Method::GET,
            "/v2/agent/toolchain/artifact",
            &HashMap::from([
                ("version".into(), act::PIN.into()),
                ("target".into(), target.into()),
            ]),
            json!({}),
            agent("a"),
        )
        .await?
        .unwrap();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
        assert_eq!(gridstator::release::sha256_bytes(&bytes), tool["sha256"]);
    }
    fixture
        .request(
            Method::POST,
            "/v1/updates",
            HashMap::new(),
            json!({"nodes":["a"]}),
            admin(),
        )
        .await?;
    let mut nodes = fixture.nodes().await?;
    nodes[0]["info"]["build"]["tools"] = json!({"act":act::PIN,"managed":["act"]});
    fixture.set_nodes(nodes).await?;
    updates::tick(fixture.controller.clone()).await?;
    assert_eq!(fixture.nodes().await?[0]["update"]["state"], "current");
    assert_eq!(fixture.view().await?["rollout"]["state"], "finished");
    Ok(())
}

#[tokio::test]
async fn source_015_act_override_view_drift_and_off_restore_current() -> Result<()> {
    let mut node = worker("a", gridstator::BUILD_ID, true, false);
    node["info"]["build"]["tools"] = json!({"act":act::PIN,"managed":["act"]});
    let mut config = settings("off");
    config["toolchain"]["act"] = Value::Null;
    let fixture = Fixture::new(vec![node], config).await?;
    seed_act(&fixture, act::PIN).await?;
    seed_act(&fixture, "9.9.3").await?;
    updates::tick(fixture.controller.clone()).await?;
    assert_eq!(fixture.nodes().await?[0]["update"]["state"], "current");
    fixture
        .controller
        .db
        .write(|connection| {
            let mut settings = settings("off");
            settings["toolchain"]["act"] = json!("9.9.3");
            db::put(connection, "settings", "cluster", &settings)
        })
        .await?;
    updates::tick(fixture.controller.clone()).await?;
    contains(
        &fixture.nodes().await?[0]["update"]["detail"],
        &format!("act {} → 9.9.3", act::PIN),
    );
    let view = fixture.view().await?;
    assert_eq!(view["toolchain"][0]["version"], "9.9.3");
    assert_eq!(view["toolchain"][0]["source"], "override");
    fixture
        .controller
        .db
        .write(|connection| db::put(connection, "settings", "cluster", &settings("off")))
        .await?;
    updates::tick(fixture.controller.clone()).await?;
    assert_eq!(fixture.nodes().await?[0]["update"]["state"], "current");
    Ok(())
}

#[tokio::test]
#[ignore = "downloads the actual act override/checksums from upstream; no services or workloads"]
async fn source_015_actual_act_override_is_checked_against_upstream_checksums() -> Result<()> {
    let version = "0.2.96";
    let target = gridstator::release::host_target()?;
    let mut node = worker("a", gridstator::BUILD_ID, true, false);
    let (platform, arch) = target.split_once('/').unwrap();
    node["info"]["platform"] = json!(platform);
    node["info"]["arch"] = json!(if arch == "amd64" { "x64" } else { "arm64" });
    let mut settings = settings("off");
    settings["toolchain"]["act"] = json!(version);
    let fixture = Fixture::new(vec![node], settings).await?;
    act::tick(fixture.controller.clone()).await?;
    let offer = act::offer(&fixture.controller, target).await?;
    let offered = &offer["tools"][0];
    assert_eq!(offered["state"], "ready", "{offer}");
    let file = match target {
        "linux/amd64" => "act_Linux_x86_64.tar.gz",
        "linux/arm64" => "act_Linux_arm64.tar.gz",
        "darwin/amd64" => "act_Darwin_x86_64.tar.gz",
        "darwin/arm64" => "act_Darwin_arm64.tar.gz",
        _ => unreachable!(),
    };
    let checksums = reqwest::get(format!(
        "https://github.com/nektos/act/releases/download/v{version}/checksums.txt"
    ))
    .await?
    .error_for_status()?
    .text()
    .await?;
    let expected = gridstator::release::checksum_for(&checksums, file)?;
    assert_eq!(offered["sha256"], expected);
    let path = act::artifact(&fixture.controller, version, target).await?;
    ensure!(
        gridstator::release::hash_file(&path)?.1 == expected,
        "override digest mismatch"
    );
    let installed = fixture.controller.dir.join("verified-act");
    let executable_digest = act::install_archive(&path, &installed, target)?;
    let output = std::process::Command::new(&installed)
        .arg("--version")
        .output()?;
    ensure!(output.status.success(), "act version probe failed");
    let reported = String::from_utf8(output.stdout)?;
    ensure!(
        reported.contains(version),
        "unexpected act version: {reported}"
    );
    eprintln!("Verified actual managed act override {version} target {target} sha256 {expected}");
    eprintln!(
        "Installed act executable sha256 {executable_digest}; {}",
        reported.trim()
    );
    if let Some(directory) = std::env::var_os("GRIDSTATOR_UPDATE_SOURCE_EVIDENCE") {
        let directory = std::path::PathBuf::from(directory);
        gridstator::state::private_dir(&directory)?;
        std::fs::write(directory.join("upstream-checksums.txt"), checksums)?;
        gridstator::state::write_json(
            &directory.join("actual-act-override.json"),
            &json!({"version":version,"target":target,"source":"override","checksumsUrl":format!("https://github.com/nektos/act/releases/download/v{version}/checksums.txt"),"archiveName":file,"archiveSha256":expected,"archiveSize":std::fs::metadata(path)?.len(),"executableSha256":executable_digest,"reportedVersion":reported.trim(),"scope":"Actual controller managed mirror download, independent upstream checksum comparison, safe native extraction and host act --version; no Docker workload or Mac tool readiness claim"}),
        )?;
    }
    Ok(())
}
