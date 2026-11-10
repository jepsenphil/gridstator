//! Native update coordination. Durable rollout decisions are separate from downloads and staging.
pub mod act;
use crate::{
    controller::Controller,
    db,
    install::InstallStore,
    release::{ArtifactMirror, ReleaseManifest},
    state,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    body::Body,
    http::Method,
    response::{IntoResponse, Response},
};
use chrono::Timelike;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const CONFIRM_SECONDS: u64 = 600;
const UPDATE_TIMEOUT_MS: i64 = 15 * 60 * 1000;
const RECHECK_MS: i64 = 60 * 1000;
const UPDATE_ONLY_FRESH_MS: i64 = 3 * 60 * 1000;
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEffect {
    Check(String),
    Cancel(String, String),
}
fn empty() -> Value {
    json!({"target":crate::BUILD_ID,"rollout":null,"failures":{}})
}
fn default_settings() -> Value {
    json!({"autoUpdate":"follow","updateWindow":null,"toolchain":{"act":null}})
}

fn current(node: &Value, target: &str) -> bool {
    node["info"]["build"]["id"] == target
}
fn exact_current(node: &Value, target: &Value) -> bool {
    current(node, target["id"].as_str().unwrap_or("")) && tool_gaps(node, target).is_empty()
}
fn tool_gaps(node: &Value, target: &Value) -> Vec<String> {
    if !managed(node)
        || node["info"]["build"]["tools"]["overrides"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool == "act"))
    {
        return Vec::new();
    }
    let key = crate::release::target_for(
        node["info"]["platform"].as_str().unwrap_or(""),
        node["info"]["arch"].as_str().unwrap_or(""),
    );
    let Some(expected) = key.and_then(|key| target["_act"][key].as_str()) else {
        return Vec::new();
    };
    let tools = &node["info"]["build"]["tools"];
    let have = tools["act"].as_str();
    let managed = tools["managed"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool == "act"));
    if managed && have == Some(expected) {
        Vec::new()
    } else {
        vec![format!(
            "act {}{} → {expected}",
            have.unwrap_or("missing"),
            if have.is_some() && !managed {
                " (system)"
            } else {
                ""
            }
        )]
    }
}
fn online(node: &Value) -> bool {
    node["status"] == "online"
}
fn managed(node: &Value) -> bool {
    node["info"]["build"]["managed"] == true
}
fn update_only(node: &Value, now: i64) -> bool {
    node["updateOnly"]["at"]
        .as_i64()
        .is_some_and(|at| now - at < UPDATE_ONLY_FRESH_MS)
}
fn mode<'a>(node: &'a Value, settings: &'a Value) -> &'a str {
    match node["settings"]["autoUpdate"].as_str() {
        Some("inherit") | None => settings["autoUpdate"].as_str().unwrap_or("follow"),
        Some(mode) => mode,
    }
}
fn clock(text: &str) -> Option<u32> {
    let (hour, minute) = text.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let hour = hour.parse::<u32>().ok()?;
    let minute = minute.parse::<u32>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}
pub fn in_window(window: &Value, minute: u32) -> bool {
    let Some(start) = window["start"].as_str().and_then(clock) else {
        return false;
    };
    let Some(end) = window["end"].as_str().and_then(clock) else {
        return false;
    };
    if start == end {
        return false;
    }
    if start < end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}
fn pending(node: &Value, auto: bool) -> Value {
    json!({"nodeId":node["id"],"name":node["name"],"state":"pending","detail":null,"auto":auto,"startedAt":null,"finishedAt":null})
}
fn rollout(target: &Value, actor: &str, now: i64) -> Value {
    json!({"id":state::id(),"target":{"id":target["id"],"version":target["version"],"sha256":target["sha256"]},"state":"running","haltReason":null,"startedBy":actor,"startedAt":now,"finishedAt":null,"now":false,"drainTimeoutSeconds":null,"nodes":[]})
}
fn restore(node: &mut Value, entry: &Value) {
    if node["settings"]["schedulingMode"] == "draining"
        && let Some(previous) = entry.get("previousMode")
    {
        node["settings"]["schedulingMode"] = previous.clone();
    }
}
fn halt(rollout: &mut Value, nodes: &mut [Value], reason: &str, now: i64) {
    rollout["state"] = json!("halted");
    rollout["haltReason"] = json!(reason);
    rollout["finishedAt"] = json!(now);
    for entry in rollout["nodes"].as_array_mut().into_iter().flatten() {
        if let Some(node) = nodes.iter_mut().find(|node| node["id"] == entry["nodeId"]) {
            restore(node, entry);
        }
        if entry["state"] == "draining" {
            entry["state"] = json!("pending");
            entry["startedAt"] = Value::Null;
        }
    }
}
fn finish(
    entry: &mut Value,
    node: Option<&mut Value>,
    state: &str,
    detail: Option<&str>,
    now: i64,
) {
    if let Some(node) = node {
        restore(node, entry);
    }
    entry["state"] = json!(state);
    entry["detail"] = json!(detail);
    entry["finishedAt"] = json!(now);
    entry.as_object_mut().unwrap().remove("deadline");
}

/// Pure policy reducer: native tests can inject time and node facts without services or restarts.
pub fn advance(
    persisted: &mut Value,
    nodes: &mut [Value],
    settings: &Value,
    target: &Value,
    available: bool,
    now: i64,
    minute: u32,
) -> Result<Vec<UpdateEffect>> {
    if persisted["target"] != target["id"] {
        if let Some(entries) = persisted["rollout"]["nodes"].as_array() {
            for entry in entries {
                if let Some(node) = nodes.iter_mut().find(|node| node["id"] == entry["nodeId"]) {
                    restore(node, entry);
                }
            }
        }
        *persisted = empty();
        persisted["target"] = target["id"].clone();
    }
    let window = in_window(&settings["updateWindow"], minute);
    if persisted["rollout"]["state"] == "running" {
        persisted["rollout"]["nodes"]
            .as_array_mut()
            .unwrap()
            .retain(|entry| {
                entry["auto"] != true
                    || entry["state"] != "pending"
                    || nodes
                        .iter()
                        .find(|node| node["id"] == entry["nodeId"])
                        .is_some_and(|node| mode(node, settings) != "off")
            });
    }
    if available && persisted["rollout"]["state"] != "halted" {
        let candidates = nodes
            .iter()
            .filter(|node| {
                let effective = mode(node, settings);
                effective != "off"
                    && (effective != "window" || window)
                    && persisted["failures"]
                        .get(node["id"].as_str().unwrap_or(""))
                        .is_none()
                    && ((online(node) && managed(node) && !exact_current(node, target))
                        || update_only(node, now))
            })
            .cloned()
            .collect::<Vec<_>>();
        for node in candidates {
            let existing = persisted["rollout"]["nodes"]
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry["nodeId"] == node["id"]));
            if persisted["rollout"]["state"] == "running"
                && existing
                    .is_some_and(|entry| entry["state"] != "done" && entry["state"] != "skipped")
            {
                continue;
            }
            if persisted["rollout"]["state"] != "running" {
                persisted["rollout"] = rollout(target, "auto-update", now);
            }
            let entries = persisted["rollout"]["nodes"].as_array_mut().unwrap();
            if let Some(entry) = entries
                .iter_mut()
                .find(|entry| entry["nodeId"] == node["id"])
            {
                *entry = pending(&node, true);
            } else {
                entries.push(pending(&node, true));
            }
        }
    }
    let mut effects = Vec::new();
    for _ in 0..4 {
        if persisted["rollout"]["state"] != "running" {
            break;
        }
        let mut r = persisted["rollout"].clone();
        let mut changed = false;
        let immediate = r["now"] == true;
        let drain_timeout = r["drainTimeoutSeconds"].as_i64();
        let active = r["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .position(|entry| entry["state"] == "draining" || entry["state"] == "updating");
        if let Some(index) = active {
            let entry = &mut r["nodes"][index];
            let node_index = nodes.iter().position(|node| node["id"] == entry["nodeId"]);
            if let Some(node_index) = node_index {
                let node = &mut nodes[node_index];
                let build = node["info"]["build"].clone();
                if online(node) && exact_current(node, target) {
                    let id = node["id"].as_str().unwrap().to_owned();
                    finish(entry, Some(node), "done", None, now);
                    persisted["failures"].as_object_mut().unwrap().remove(&id);
                    changed = true;
                } else if online(node)
                    && build["lastRollback"]["from"] == target["id"]
                    && build["lastRollback"]["at"].as_i64().unwrap_or(0)
                        >= entry["startedAt"].as_i64().unwrap_or(i64::MAX)
                {
                    let reason = format!(
                        "{} {}; the launcher rolled back to {}",
                        target["id"].as_str().unwrap_or("candidate"),
                        build["lastRollback"]["reason"].as_str().unwrap_or("failed"),
                        build["lastRollback"]["to"]
                            .as_str()
                            .unwrap_or("previous build")
                    );
                    let id = node["id"].as_str().unwrap().to_owned();
                    let name = node["name"].as_str().unwrap_or(&id).to_owned();
                    finish(entry, Some(node), "failed", Some(&reason), now);
                    persisted["failures"][&id] =
                        json!({"target":target["id"],"error":reason,"at":now});
                    halt(&mut r, nodes, &format!("{name}: {reason}"), now);
                    changed = true;
                } else if entry["state"] == "draining" {
                    if !online(node) {
                        finish(
                            entry,
                            Some(node),
                            "skipped",
                            Some("went offline while draining"),
                            now,
                        );
                        changed = true;
                    } else if node["activeJobs"].as_array().is_none_or(Vec::is_empty) {
                        entry["state"] = json!("updating");
                        entry["deadline"] = json!(now + UPDATE_TIMEOUT_MS);
                        entry["nudgedAt"] = json!(now);
                        effects.push(UpdateEffect::Check(node["id"].as_str().unwrap().into()));
                        changed = true;
                    } else {
                        let timed_out = drain_timeout.is_some_and(|seconds| {
                            now - entry["startedAt"].as_i64().unwrap_or(now) > seconds * 1000
                        });
                        if (immediate || timed_out) && entry["cancelled"] != true {
                            entry["cancelled"] = json!(true);
                            entry["detail"] = json!(if immediate {
                                "cancelling running jobs (--now)"
                            } else {
                                "drain timed out; cancelling running jobs"
                            });
                            effects.push(UpdateEffect::Cancel(
                                node["id"].as_str().unwrap().into(),
                                format!(
                                    "{} is being updated",
                                    node["name"].as_str().unwrap_or("worker")
                                ),
                            ));
                            changed = true;
                        }
                    }
                } else if entry["deadline"]
                    .as_i64()
                    .is_some_and(|deadline| now > deadline)
                {
                    let id = node["id"].as_str().unwrap().to_owned();
                    let name = node["name"].as_str().unwrap_or(&id).to_owned();
                    let reason = format!(
                        "did not come back on {} within {}m",
                        target["id"].as_str().unwrap_or("candidate"),
                        UPDATE_TIMEOUT_MS / 60_000
                    );
                    finish(entry, Some(node), "failed", Some(&reason), now);
                    persisted["failures"][&id] =
                        json!({"target":target["id"],"error":reason,"at":now});
                    halt(&mut r, nodes, &format!("{name}: {reason}"), now);
                    changed = true;
                } else if online(node) && now - entry["nudgedAt"].as_i64().unwrap_or(0) > RECHECK_MS
                {
                    entry["nudgedAt"] = json!(now);
                    effects.push(UpdateEffect::Check(node["id"].as_str().unwrap().into()));
                    changed = true;
                }
            } else {
                finish(entry, None, "skipped", Some("node was removed"), now);
                changed = true;
            }
        } else {
            let next = r["nodes"].as_array().unwrap().iter().position(|entry| {
                entry["state"] == "pending"
                    && (entry["auto"] != true
                        || nodes
                            .iter()
                            .find(|node| node["id"] == entry["nodeId"])
                            .is_none_or(|node| mode(node, settings) != "window" || window))
            });
            if let Some(index) = next {
                let entry = &mut r["nodes"][index];
                if let Some(node) = nodes.iter_mut().find(|node| node["id"] == entry["nodeId"]) {
                    if exact_current(node, target) && online(node) {
                        finish(entry, Some(node), "done", Some("already current"), now);
                    } else if online(node) && managed(node) {
                        entry["previousMode"] = node["settings"]["schedulingMode"].clone();
                        node["settings"]["schedulingMode"] = json!("draining");
                        entry["state"] = json!("draining");
                        entry["startedAt"] = json!(now);
                    } else if update_only(node, now) {
                        entry["state"] = json!("updating");
                        entry["startedAt"] = json!(now);
                        entry["deadline"] = json!(now + UPDATE_TIMEOUT_MS);
                    } else {
                        let reason = if online(node) {
                            "not a managed install"
                        } else {
                            "offline"
                        };
                        finish(entry, Some(node), "skipped", Some(reason), now);
                    }
                } else {
                    finish(entry, None, "skipped", Some("node was removed"), now);
                }
                changed = true;
            } else if !r["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["state"] == "pending")
            {
                r["state"] = json!("finished");
                r["finishedAt"] = json!(now);
                changed = true;
            }
        }
        persisted["rollout"] = r;
        if !changed {
            break;
        }
    }
    for node in nodes {
        let id = node["id"].as_str().unwrap_or("");
        let entry = persisted["rollout"]["nodes"]
            .as_array()
            .and_then(|entries| entries.iter().find(|entry| entry["nodeId"] == id));
        let active = persisted["rollout"]["state"] == "running"
            && entry
                .is_some_and(|entry| entry["state"] == "draining" || entry["state"] == "updating");
        let failure = persisted["failures"].get(id);
        let status = if active {
            "updating"
        } else if !online(node) && !update_only(node, now) {
            "unknown"
        } else if exact_current(node, target) {
            "current"
        } else if failure.is_some() {
            "failed"
        } else {
            "behind"
        };
        let detail = if active {
            if entry.is_some_and(|entry| entry["state"] == "draining") {
                json!(format!(
                    "draining before updating to {} ({} job(s) left)",
                    target["id"].as_str().unwrap_or("candidate"),
                    node["activeJobs"].as_array().map_or(0, Vec::len)
                ))
            } else {
                json!(format!(
                    "restarting on {}",
                    target["id"].as_str().unwrap_or("candidate")
                ))
            }
        } else if status == "unknown" {
            json!(node["info"]["build"]["id"].as_str().map_or_else(
                || "offline".into(),
                |build| format!("offline; last reported {build}")
            ))
        } else if status == "failed" {
            failure.unwrap()["error"].clone()
        } else if status == "behind" {
            let reason = if !node["info"]["build"].is_object() && !update_only(node, now) {
                format!(
                    "agent {} predates self-update; install the new version on it once",
                    node["info"]["agentVersion"].as_str().unwrap_or("?")
                )
            } else if !managed(node) && !update_only(node, now) {
                "not a managed install; install this worker as a verified native managed service to enable updates".into()
            } else if !available {
                "controller native release archives are unavailable".into()
            } else if entry.is_some_and(|entry| entry["state"] == "pending") {
                if persisted["rollout"]["state"] == "halted" {
                    format!(
                        "rollout halted: {}",
                        persisted["rollout"]["haltReason"]
                            .as_str()
                            .unwrap_or("unknown")
                    )
                } else {
                    "waiting for its turn in the rollout".into()
                }
            } else if mode(node, settings) == "off" {
                "auto-update is off; run gridstator agents update".into()
            } else if mode(node, settings) == "window" {
                let window = &settings["updateWindow"];
                if window.is_object() {
                    format!(
                        "waiting for the update window ({}-{})",
                        window["start"].as_str().unwrap_or("?"),
                        window["end"].as_str().unwrap_or("?")
                    )
                } else {
                    "auto-update mode is window, but no update window is set".into()
                }
            } else {
                "update pending".into()
            };
            let gaps = if current(node, target["id"].as_str().unwrap_or("")) {
                tool_gaps(node, target)
            } else {
                Vec::new()
            };
            json!(if gaps.is_empty() {
                reason
            } else {
                format!("toolchain {}; {reason}", gaps.join(", "))
            })
        } else {
            Value::Null
        };
        node["update"] = json!({"state":status,"build":node["info"]["build"].get("id").cloned().unwrap_or(Value::Null),"detail":detail,"autoUpdate":mode(node,settings),"canUpdate":managed(node)||update_only(node,now)});
    }
    Ok(effects)
}
fn mirror(dir: &Path) -> ArtifactMirror {
    ArtifactMirror::new(dir.join("native-release-mirror"))
}
fn target_view(dir: &Path) -> (Value, bool) {
    let mirror = mirror(dir);
    let manifest = mirror.manifest(crate::BUILD_ID).ok();
    let available = manifest.as_ref().is_some_and(|manifest| {
        manifest.artifacts.iter().all(|(target, artifact)| {
            mirror
                .artifact_path(manifest, target)
                .ok()
                .and_then(|path| fs::metadata(path).ok())
                .is_some_and(|metadata| {
                    metadata.is_file() && metadata.len() == artifact.archive.size
                })
        })
    });
    let capability = managed_store().is_ok();
    let rollback = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
        .and_then(|root| {
            InstallStore::new(PathBuf::from(root))
                .state()
                .ok()
                .flatten()
        })
        .and_then(|state| state.last_rollback);
    (
        json!({"id":crate::BUILD_ID,"version":crate::VERSION,"runtime":"rust","target":crate::release::host_target().ok(),"sha256":manifest.as_ref().and_then(|manifest|manifest.target(crate::release::host_target().ok()?).ok()).map(|artifact|artifact.executable.sha256.clone()),"updatable":available,"reason":if available{None}else{Some("This build has no verified four-target native release archives in the controller mirror")},"lastRollback":rollback,"selfUpdate":{"available":capability,"reason":if capability{None}else{Some("Install this controller as a verified native managed service to enable self-update")}}}),
        available,
    )
}
pub fn controller_build_view(directory: &Path) -> Value {
    target_view(directory).0
}

fn managed_store() -> Result<InstallStore> {
    ensure!(
        std::env::var("GRIDSTATOR_SUPERVISED").as_deref() == Ok("1"),
        "CONFLICT: controller is not a managed install; run it under the native managed supervisor"
    );
    ensure!(
        std::env::var("GRIDSTATOR_INSTALL_VERSION").as_deref() == Ok(crate::BUILD_ID),
        "CONFLICT: launcher build differs from embedded native build"
    );
    let root = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
        .context("CONFLICT: native install root is unavailable")?;
    let store = InstallStore::new(PathBuf::from(root));
    let installed = store
        .verify_running_payload()
        .context("CONFLICT: this executable is not the verified active native managed payload")?;
    ensure!(
        installed.state.current == crate::BUILD_ID,
        "CONFLICT: running binary differs from active managed build"
    );
    Ok(store)
}
async fn managed_store_background(owner: Option<Arc<Controller>>) -> Result<InstallStore> {
    tokio::task::spawn_blocking(move || {
        let _owner = owner;
        managed_store()
    })
    .await?
}
fn public_rollout(mut rollout: Value) -> Value {
    if let Some(entries) = rollout.get_mut("nodes").and_then(Value::as_array_mut) {
        for entry in entries {
            if let Some(object) = entry.as_object_mut() {
                object.retain(|key, _| {
                    [
                        "nodeId",
                        "name",
                        "state",
                        "detail",
                        "auto",
                        "startedAt",
                        "finishedAt",
                    ]
                    .contains(&key.as_str())
                });
            }
        }
    }
    rollout
}
async fn view(controller: &Controller) -> Result<Value> {
    let directory = controller.dir.clone();
    let (target, _) = tokio::task::spawn_blocking(move || target_view(&directory)).await?;
    let (settings, persisted, controller_update, preparation) = controller
        .db
        .call(|connection| {
            Ok((
                db::get(connection, "settings", "cluster")?.unwrap_or_else(default_settings),
                db::get(connection, "updates", "rollout")?.unwrap_or_else(empty),
                db::get(connection, "updates", "controller")?.unwrap_or(Value::Null),
                db::get(connection, "updates", "mirrorPreparation")?.unwrap_or(Value::Null),
            ))
        })
        .await?;
    Ok(
        json!({"controller":target,"autoUpdate":settings["autoUpdate"].as_str().unwrap_or("follow"),"updateWindow":settings.get("updateWindow").cloned().unwrap_or(Value::Null),"rollout":public_rollout(persisted["rollout"].clone()),"toolchain":act::view(controller).await?,"controllerUpdate":controller_update,"mirrorPreparation":preparation}),
    )
}
async fn coordinate_workers(controller: Arc<Controller>) -> Result<()> {
    let directory = controller.dir.clone();
    let (mut target, available) =
        tokio::task::spawn_blocking(move || target_view(&directory)).await?;
    target["_act"] = act::expected(&controller).await?;
    let now = state::now();
    let time = chrono::Local::now();
    let minute = time.hour() * 60 + time.minute();
    let effects = controller
        .db
        .write(move |connection| {
            let mut persisted = db::get(connection, "updates", "rollout")?.unwrap_or_else(empty);
            let mut nodes = db::list(connection, "nodes")?;
            let settings =
                db::get(connection, "settings", "cluster")?.unwrap_or_else(default_settings);
            let effects = advance(
                &mut persisted,
                &mut nodes,
                &settings,
                &target,
                available,
                now,
                minute,
            )?;
            db::put(connection, "updates", "rollout", &persisted)?;
            for node in nodes {
                db::put(connection, "nodes", node["id"].as_str().unwrap(), &node)?;
            }
            Ok(effects)
        })
        .await?;
    for effect in effects {
        match effect {
            UpdateEffect::Check(node) => {
                controller.send_agent(&node, json!({"type":"update.check"}));
            }
            UpdateEffect::Cancel(node, reason) => {
                controller.cancel_node_jobs(&node, &reason).await?
            }
        }
    }
    Ok(())
}
fn admin(principal: &Value) -> Result<()> {
    ensure!(
        principal["kind"] == "client" && principal["role"] == "admin",
        "FORBIDDEN: administrator required"
    );
    Ok(())
}
fn agent(principal: &Value) -> Result<&str> {
    ensure!(
        principal["kind"] == "agent",
        "FORBIDDEN: agent credential required"
    );
    principal["nodeId"]
        .as_str()
        .context("FORBIDDEN: agent has no node identity")
}

pub async fn route(
    controller: Arc<Controller>,
    method: Method,
    path: &str,
    query: &HashMap<String, String>,
    input: Value,
    principal: Value,
) -> Result<Option<Response>> {
    let result = match (method.as_str(), path) {
        ("GET", "/v1/updates") => {
            ensure!(
                principal["kind"] == "client",
                "FORBIDDEN: client credential required"
            );
            view(&controller).await?
        }
        ("POST", "/v1/updates") => {
            admin(&principal)?;
            ensure_current_mirror(controller.clone()).await?;
            let directory = controller.dir.clone();
            let (mut target, available) =
                tokio::task::spawn_blocking(move || target_view(&directory)).await?;
            target["_act"] = act::expected(&controller).await?;
            ensure!(
                available,
                "CONFLICT: workers cannot update until the controller has verified native release archives"
            );
            let now_flag = input.get("now").cloned().unwrap_or(json!(false));
            ensure!(now_flag.is_boolean(), "BAD_REQUEST: now must be boolean");
            let deadline = input
                .get("drainTimeoutSeconds")
                .cloned()
                .unwrap_or(Value::Null);
            ensure!(
                deadline.is_null()
                    || deadline
                        .as_i64()
                        .is_some_and(|seconds| (1..=604800).contains(&seconds)),
                "BAD_REQUEST: drainTimeoutSeconds must be null or 1..604800"
            );
            let names = input.get("nodes").cloned();
            if let Some(names) = &names {
                ensure!(
                    names.as_array().is_some_and(
                        |names| names.len() <= 1000 && names.iter().all(Value::is_string)
                    ),
                    "BAD_REQUEST: nodes must be a string array"
                );
            }
            let actor = principal["name"].as_str().unwrap_or("admin").to_owned();
            let now = state::now();
            controller
                .db
                .write(move |connection| {
                    let nodes = db::list(connection, "nodes")?;
                    let selected = if let Some(names) = names {
                        let mut selected = Vec::new();
                        for name in names.as_array().unwrap() {
                            selected.push(
                                nodes
                                    .iter()
                                    .find(|node| node["id"] == *name || node["name"] == *name)
                                    .cloned()
                                    .context("NOT_FOUND: selected node does not exist")?,
                            );
                        }
                        selected
                    } else {
                        nodes
                            .into_iter()
                            .filter(|node| online(node) && !exact_current(node, &target))
                            .collect()
                    };
                    let mut persisted =
                        db::get(connection, "updates", "rollout")?.unwrap_or_else(empty);
                    if persisted["rollout"]["state"] != "running" {
                        persisted["rollout"] = rollout(&target, &actor, now);
                    }
                    persisted["target"] = target["id"].clone();
                    persisted["rollout"]["now"] =
                        json!(persisted["rollout"]["now"] == true || now_flag == true);
                    persisted["rollout"]["drainTimeoutSeconds"] = deadline;
                    for node in selected {
                        if exact_current(&node, &target) && online(&node) {
                            continue;
                        }
                        let id = node["id"].as_str().unwrap();
                        persisted["failures"].as_object_mut().unwrap().remove(id);
                        let entries = persisted["rollout"]["nodes"].as_array_mut().unwrap();
                        if let Some(entry) = entries.iter_mut().find(|entry| entry["nodeId"] == id)
                        {
                            if entry["state"] != "draining" && entry["state"] != "updating" {
                                *entry = pending(&node, false);
                            }
                        } else {
                            entries.push(pending(&node, false));
                        }
                    }
                    db::put(connection, "updates", "rollout", &persisted)
                })
                .await?;
            coordinate_workers(controller.clone()).await?;
            view(&controller).await?
        }
        ("POST", "/v1/updates/cancel") => {
            admin(&principal)?;
            let actor = principal["name"].as_str().unwrap_or("admin").to_owned();
            controller
                .db
                .write(move |connection| {
                    let mut persisted =
                        db::get(connection, "updates", "rollout")?.unwrap_or_else(empty);
                    ensure!(
                        persisted["rollout"]["state"] == "running",
                        "CONFLICT: no worker update is running"
                    );
                    let mut nodes = db::list(connection, "nodes")?;
                    halt(
                        &mut persisted["rollout"],
                        &mut nodes,
                        &format!("stopped by {actor}"),
                        state::now(),
                    );
                    for node in nodes {
                        db::put(connection, "nodes", node["id"].as_str().unwrap(), &node)?;
                    }
                    db::put(connection, "updates", "rollout", &persisted)
                })
                .await?;
            coordinate_workers(controller.clone()).await?;
            view(&controller).await?
        }
        ("GET", "/v2/agent/update") => {
            let id = agent(&principal)?.to_owned();
            let mode = query.get("mode").map(String::as_str).unwrap_or("normal");
            ensure!(
                ["normal", "update-only"].contains(&mode),
                "BAD_REQUEST: invalid update mode"
            );
            let running = query.get("current").cloned().unwrap_or_default();
            let target = query
                .get("target")
                .context("BAD_REQUEST: target required")?;
            ensure!(
                crate::release::SUPPORTED_TARGETS.contains(&target.as_str()),
                "BAD_REQUEST: unsupported native target"
            );
            if mode == "update-only" {
                let selected = id.clone();
                let current = running.clone();
                let native_target = target.clone();
                controller
                    .db
                    .write(move |connection| {
                        let mut node = db::get(connection, "nodes", &selected)?
                            .context("NOT_FOUND: worker")?;
                        node["updateOnly"] =
                            json!({"current":current,"target":native_target,"at":state::now()});
                        db::put(connection, "nodes", &selected, &node)
                    })
                    .await?;
                coordinate_workers(controller.clone()).await?;
            }
            let persisted = controller
                .db
                .call(|connection| {
                    Ok(db::get(connection, "updates", "rollout")?.unwrap_or_else(empty))
                })
                .await?;
            let scheduled = persisted["rollout"]["state"] == "running"
                && persisted["rollout"]["nodes"]
                    .as_array()
                    .is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|entry| entry["nodeId"] == id && entry["state"] == "updating")
                    });
            if running == crate::BUILD_ID {
                json!({"action":"none","reason":"already current"})
            } else if !scheduled {
                json!({"action":"none","reason":"not scheduled for update"})
            } else {
                let manifest = mirror(&controller.dir).manifest(crate::BUILD_ID)?;
                let artifact = manifest.target(target)?;
                json!({"action":"update","buildId":manifest.build_id,"manifest":manifest,"manifestSha256":manifest.sha256()?,"artifact":{"path":format!("/v2/agent/update/artifact?buildId={}&target={target}",manifest.build_id),"sha256":artifact.archive.sha256,"size":artifact.archive.size},"confirmWithinSeconds":CONFIRM_SECONDS})
            }
        }
        ("GET", "/v2/agent/update/artifact") => {
            agent(&principal)?;
            let build = query
                .get("buildId")
                .context("BAD_REQUEST: buildId required")?;
            ensure!(
                build == crate::BUILD_ID,
                "CONFLICT: artifact is not the controller build"
            );
            let target = query
                .get("target")
                .context("BAD_REQUEST: target required")?;
            let directory = controller.dir.clone();
            let target = target.clone();
            let path = tokio::task::spawn_blocking(move || {
                let mirror = mirror(&directory);
                let manifest = mirror.manifest(crate::BUILD_ID)?;
                mirror.verified_artifact(&manifest, &target)
            })
            .await??;
            let file = tokio::fs::File::open(path).await?;
            return Ok(Some(
                Response::builder()
                    .header("content-type", "application/gzip")
                    .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))?,
            ));
        }
        ("GET", "/v2/agent/toolchain") => {
            agent(&principal)?;
            act::offer(
                &controller,
                query
                    .get("target")
                    .context("BAD_REQUEST: native tool target required")?,
            )
            .await?
        }
        ("GET", "/v2/agent/toolchain/artifact") => {
            agent(&principal)?;
            let path = act::artifact(
                &controller,
                query
                    .get("version")
                    .context("BAD_REQUEST: act version required")?,
                query
                    .get("target")
                    .context("BAD_REQUEST: native tool target required")?,
            )
            .await?;
            let file = tokio::fs::File::open(path).await?;
            return Ok(Some(
                Response::builder()
                    .header("content-type", "application/gzip")
                    .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))?,
            ));
        }
        ("POST", "/v2/agent/update/report") => {
            let id = agent(&principal)?.to_owned();
            let target = input["target"]
                .as_str()
                .context("BAD_REQUEST: update report target required")?
                .to_owned();
            let error = input["error"]
                .as_str()
                .context("BAD_REQUEST: update report error required")?
                .to_owned();
            ensure!(error.len() <= 4096, "BAD_REQUEST: update report too long");
            if target == crate::BUILD_ID {
                controller
                    .db
                    .write(move |connection| {
                        let mut persisted =
                            db::get(connection, "updates", "rollout")?.unwrap_or_else(empty);
                        persisted["failures"][&id] =
                            json!({"target":target,"error":error,"at":state::now()});
                        let mut nodes = db::list(connection, "nodes")?;
                        if persisted["rollout"]["state"] == "running" {
                            let mut failed_name = None;
                            for entry in persisted["rollout"]["nodes"].as_array_mut().unwrap() {
                                if entry["nodeId"] == id
                                    && (entry["state"] == "draining"
                                        || entry["state"] == "updating")
                                {
                                    failed_name =
                                        Some(entry["name"].as_str().unwrap_or(&id).to_owned());
                                    finish(
                                        entry,
                                        nodes.iter_mut().find(|node| node["id"] == id),
                                        "failed",
                                        Some(&error),
                                        state::now(),
                                    );
                                }
                            }
                            if let Some(name) = failed_name {
                                halt(
                                    &mut persisted["rollout"],
                                    &mut nodes,
                                    &format!("{name}: {error}"),
                                    state::now(),
                                );
                            }
                        }
                        for node in nodes {
                            db::put(connection, "nodes", node["id"].as_str().unwrap(), &node)?;
                        }
                        db::put(connection, "updates", "rollout", &persisted)
                    })
                    .await?;
                coordinate_workers(controller.clone()).await?;
            }
            json!({"ok":true})
        }
        ("GET", "/v1/releases") => {
            ensure!(
                principal["kind"] == "client",
                "FORBIDDEN: client credential required"
            );
            let releases = controller
                .releases
                .list(query.get("prerelease").is_some_and(|value| value == "true"))
                .await?;
            let releases = releases
                .into_iter()
                .map(|release| {
                    let mut value = serde_json::to_value(release).expect("release JSON");
                    value["packageName"] = value["manifestName"].clone();
                    value
                })
                .collect::<Vec<_>>();
            json!({"releases":releases})
        }
        _ if method == Method::POST
            && path.starts_with("/v1/releases/")
            && path.ends_with("/install") =>
        {
            admin(&principal)?;
            let tag = path
                .trim_start_matches("/v1/releases/")
                .trim_end_matches("/install");
            crate::release::validate_release_tag(tag)?;
            let store = managed_store_background(Some(controller.clone())).await?;
            let source = controller
                .releases
                .clone()
                .with_lifetime_guard(controller.clone());
            let _install = source.installation_guard()?;
            let release = source.fetch_manifest(tag).await?;
            let selected = release.manifest.clone();
            let owner = controller.clone();
            tokio::task::spawn_blocking(move || {
                let _owner = owner;
                ensure_active_manifest(&selected, &store.manifest(crate::BUILD_ID)?)
            })
            .await??;
            let mirror = mirror(&controller.dir);
            for target in crate::release::SUPPORTED_TARGETS {
                source.mirror_artifact(&release, target, &mirror).await?;
            }
            schedule_controller(controller.clone(), &release.manifest).await?
        }
        _ => return Ok(None),
    };
    Ok(Some(Json(result).into_response()))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControllerSwitch {
    from: String,
    to: String,
    at: i64,
    snapshot: PathBuf,
    embedded_node: Option<String>,
    previous_mode: Option<Value>,
}
fn switch_file(directory: &Path) -> PathBuf {
    directory.join("updates/controller-switch.json")
}

/// Restore the pre-candidate SQLite database before opening it with the old executable.
pub fn recover_before_open(directory: &Path) -> Result<()> {
    let path = switch_file(directory);
    if !path.exists() {
        return Ok(());
    }
    let record: ControllerSwitch = serde_json::from_slice(&fs::read(&path)?)?;
    crate::release::validate_build_id(&record.from)?;
    crate::release::validate_build_id(&record.to)?;
    if record.to == crate::BUILD_ID {
        return Ok(());
    }
    let root = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| directory.join("install"));
    let install = InstallStore::new(root).state()?;
    let rolled_back = record.from == crate::BUILD_ID
        && install
            .as_ref()
            .and_then(|state| state.last_rollback.as_ref())
            .is_some_and(|rollback| {
                rollback.from == record.to
                    && rollback.to == record.from
                    && rollback.at >= record.at as u64
            });
    if rolled_back {
        ensure!(
            record.snapshot.is_file()
                && fs::canonicalize(&record.snapshot)?
                    .starts_with(fs::canonicalize(directory.join("updates"))?),
            "Rollback database snapshot is unavailable or outside native updates directory"
        );
        let database = directory.join("controller.db");
        let failed = directory.join(format!(
            "controller.db.failed-{}-{}",
            record.to,
            state::id()
        ));
        if database.exists() {
            fs::rename(&database, &failed)?;
        }
        for suffix in ["-wal", "-shm"] {
            let source = PathBuf::from(format!("{}{suffix}", database.display()));
            if source.exists() {
                fs::rename(source, format!("{}{suffix}", failed.display()))?;
            }
        }
        let copied = tempfile::NamedTempFile::new_in(directory)?;
        fs::copy(&record.snapshot, copied.path())?;
        copied.as_file().sync_all()?;
        copied.persist(&database).map_err(|error| error.error)?;
        let connection = rusqlite::Connection::open(&database)?;
        db::delete(&connection, "updates", "controller")?;
        if let (Some(id), Some(mode)) = (&record.embedded_node, &record.previous_mode)
            && let Some(mut node) = db::get(&connection, "nodes", id)?
        {
            node["settings"]["schedulingMode"] = mode.clone();
            db::put(&connection, "nodes", id, &node)?;
        }
        eprintln!(
            "Restored pre-update database after rollback; failed candidate database retained at {}",
            failed.display()
        );
    }
    if rolled_back || record.from != crate::BUILD_ID {
        fs::remove_file(path)?;
    }
    Ok(())
}
pub fn confirm_controller(directory: &Path) -> Result<()> {
    let Some(root) = std::env::var_os("GRIDSTATOR_INSTALL_ROOT") else {
        return Ok(());
    };
    if std::env::var("GRIDSTATOR_SUPERVISED").as_deref() != Ok("1") {
        return Ok(());
    }
    let confirmation = InstallStore::new(PathBuf::from(root)).confirm_running_payload()?;
    if let Some(error) = confirmation.refresh_error {
        eprintln!("Healthy controller confirmed; native supervisor refresh failed: {error}");
    }
    let path = switch_file(directory);
    if path.exists() {
        let record: ControllerSwitch = serde_json::from_slice(&fs::read(&path)?)?;
        if record.to == crate::BUILD_ID {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
async fn schedule_controller(
    controller: Arc<Controller>,
    manifest: &ReleaseManifest,
) -> Result<Value> {
    let store = managed_store_background(Some(controller.clone())).await?;
    if manifest.build_id == crate::BUILD_ID {
        return Ok(json!({"action":"none","from":crate::BUILD_ID,"to":crate::BUILD_ID}));
    }
    let artifact =
        mirror(&controller.dir).verified_artifact(manifest, crate::release::host_target()?)?;
    let selected = manifest.clone();
    let target = crate::release::host_target()?.to_owned();
    let owner = controller.clone();
    tokio::task::spawn_blocking(move || {
        let _owner = owner;
        store.install_verified(&selected, &target, &artifact)
    })
    .await??;
    let embedded = state::read_json(&controller.dir.join("embedded-agent/agent.json"))
        .ok()
        .and_then(|value| value["nodeId"].as_str().map(str::to_owned));
    let candidate = manifest.build_id.clone();
    let wanted = candidate.clone();
    controller.db.write(move|connection|{ensure!(db::get(connection,"updates","controller")?.is_none_or(|pending|pending["phase"]=="failed"),"CONFLICT: another controller update is pending");let mut previous_mode=None;if let Some(id)=&embedded&& let Some(mut node)=db::get(connection,"nodes",id)?{previous_mode=Some(node["settings"]["schedulingMode"].clone());node["settings"]["schedulingMode"]=json!("draining");db::put(connection,"nodes",id,&node)?;}db::put(connection,"updates","controller",&json!({"from":crate::BUILD_ID,"to":candidate,"phase":"draining","embeddedNode":embedded,"previousMode":previous_mode,"createdAt":state::now()}))}).await?;
    Ok(json!({"action":"restarting","from":crate::BUILD_ID,"to":wanted}))
}
async fn advance_controller(controller: Arc<Controller>) -> Result<()> {
    let pending = controller
        .db
        .call(|connection| db::get(connection, "updates", "controller"))
        .await?;
    let Some(pending) = pending else {
        return Ok(());
    };
    if pending["to"] == crate::BUILD_ID {
        controller
            .db
            .write(move |connection| {
                if let Some(id) = pending["embeddedNode"].as_str()
                    && let Some(mut node) = db::get(connection, "nodes", id)?
                    && node["settings"]["schedulingMode"] == "draining"
                    && !pending["previousMode"].is_null()
                {
                    node["settings"]["schedulingMode"] = pending["previousMode"].clone();
                    db::put(connection, "nodes", id, &node)?;
                }
                db::delete(connection, "updates", "controller")
            })
            .await?;
        return Ok(());
    }
    if pending["phase"] == "launching"
        && pending["claimPid"].as_u64() != Some(u64::from(std::process::id()))
    {
        let root = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| controller.dir.join("install"));
        if InstallStore::new(root)
            .state()?
            .is_some_and(|state| state.current == crate::BUILD_ID)
        {
            controller
                .db
                .write(|connection| {
                    if let Some(mut pending) = db::get(connection, "updates", "controller")? {
                        pending["phase"] = json!("draining");
                        db::put(connection, "updates", "controller", &pending)?;
                    }
                    Ok(())
                })
                .await?;
        }
        return Ok(());
    }
    if pending["phase"] != "draining" {
        return Ok(());
    }
    // Claim launch in the same serialized transaction that checks controller-owned job facts.
    // API ticks may overlap the periodic task, but only this claimant backs up/activates bytes.
    let pending = controller
        .db
        .write(|connection| {
            let Some(mut pending) = db::get(connection, "updates", "controller")? else {
                return Ok(None);
            };
            if pending["phase"] != "draining" {
                return Ok(None);
            }
            if let Some(id) = pending["embeddedNode"].as_str() {
                let active = db::list(connection, "jobs")?.iter().any(|job| {
                    job["nodeId"] == id
                        && matches!(
                            job["state"].as_str(),
                            Some("assigned" | "starting" | "running")
                        )
                });
                let reported = db::get(connection, "nodes", id)?.is_some_and(|node| {
                    node["activeJobs"]
                        .as_array()
                        .is_some_and(|jobs| !jobs.is_empty())
                });
                if active || reported {
                    return Ok(None);
                }
            }
            pending["phase"] = json!("launching");
            pending["claimPid"] = json!(std::process::id());
            db::put(connection, "updates", "controller", &pending)?;
            Ok(Some(pending))
        })
        .await?;
    let Some(pending) = pending else {
        return Ok(());
    };
    let candidate = pending["to"]
        .as_str()
        .context("controller candidate ID required")?
        .to_owned();
    let directory = controller.dir.clone();
    state::private_dir(&directory.join("updates"))?;
    let snapshot = directory
        .join("updates")
        .join(format!("db-before-{candidate}-{}.sqlite", state::id()));
    let snapshot_copy = snapshot.clone();
    let owner = controller.clone();
    controller
        .db
        .call(move |connection| {
            let _owner = owner;
            connection.backup("main", &snapshot_copy, None)?;
            Ok(())
        })
        .await?;
    let record = ControllerSwitch {
        from: crate::BUILD_ID.into(),
        to: candidate.clone(),
        at: state::now(),
        snapshot,
        embedded_node: pending["embeddedNode"].as_str().map(str::to_owned),
        previous_mode: pending.get("previousMode").cloned(),
    };
    state::write_json(&switch_file(&directory), &serde_json::to_value(&record)?)?;
    let store = managed_store_background(Some(controller.clone())).await?;
    let candidate_copy = candidate.clone();
    let owner = controller.clone();
    tokio::task::spawn_blocking(move || {
        let _owner = owner;
        store.activate(&candidate_copy, Some(Duration::from_secs(CONFIRM_SECONDS)))
    })
    .await??;
    controller
        .db
        .write(move |connection| {
            let mut pending = pending;
            pending["phase"] = json!("switching");
            db::put(connection, "updates", "controller", &pending)
        })
        .await?;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        controller.request_restart();
    });
    Ok(())
}

/// Import the authenticated native bundle without opening or extracting arbitrary member paths.
pub fn import_bundle(path: &Path, expected: &str, directory: &Path) -> Result<ReleaseManifest> {
    import_bundle_file(fs::File::open(path)?, expected, directory)
}
fn import_bundle_file(
    source: fs::File,
    expected: &str,
    directory: &Path,
) -> Result<ReleaseManifest> {
    import_bundle_file_with_active(source, expected, directory, None)
}
fn ensure_active_manifest(incoming: &ReleaseManifest, active: &ReleaseManifest) -> Result<()> {
    ensure!(
        incoming.build_id != active.build_id || incoming == active,
        "BAD_REQUEST: immutable active native build {} cannot be rebound to a different manifest",
        active.build_id
    );
    Ok(())
}
fn import_bundle_file_with_active(
    mut source: fs::File,
    expected: &str,
    directory: &Path,
    active: Option<&ReleaseManifest>,
) -> Result<ReleaseManifest> {
    let metadata = source.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_BUNDLE_BYTES,
        "BAD_REQUEST: native bundle exceeds size limit or is not a regular file"
    );
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        ensure!(
            size <= MAX_BUNDLE_BYTES,
            "BAD_REQUEST: native bundle exceeds size limit"
        );
        hash.update(&buffer[..count]);
    }
    let actual = hex::encode(hash.finalize());
    ensure!(
        actual == expected,
        "BAD_REQUEST: native bundle SHA-256 mismatch: expected {expected}, got {actual}"
    );
    source.rewind()?;
    let staging = tempfile::tempdir()?;
    let decoder = flate2::read::GzDecoder::new(source);
    let mut archive = tar::Archive::new(decoder);
    let mut files = HashMap::<String, PathBuf>::new();
    let mut expanded = 0u64;
    for (index, entry) in archive.entries()?.enumerate() {
        ensure!(
            index < 5,
            "BAD_REQUEST: native bundle must contain one manifest and four target archives"
        );
        let mut entry = entry?;
        ensure!(
            entry.header().entry_type().is_file(),
            "BAD_REQUEST: native bundle members must be regular files"
        );
        let name = entry
            .path()?
            .to_str()
            .context("BAD_REQUEST: bundle filename is not UTF-8")?
            .to_owned();
        ensure!(
            !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..",
            "BAD_REQUEST: unsafe native bundle member"
        );
        ensure!(
            !files.contains_key(&name),
            "BAD_REQUEST: duplicate native bundle member"
        );
        ensure!(
            entry.size() <= crate::release::MAX_ARTIFACT_BYTES,
            "BAD_REQUEST: bundle member too large"
        );
        ensure!(
            name != crate::release::RELEASE_MANIFEST_NAME
                || entry.size() <= crate::release::MAX_MANIFEST_BYTES,
            "BAD_REQUEST: native manifest exceeds its size limit"
        );
        expanded = expanded
            .checked_add(entry.size())
            .context("BAD_REQUEST: expanded bundle overflow")?;
        ensure!(
            expanded <= 4 * crate::release::MAX_ARTIFACT_BYTES + crate::release::MAX_MANIFEST_BYTES,
            "BAD_REQUEST: native bundle expansion limit exceeded"
        );
        let destination = staging.path().join(&name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        std::io::copy(&mut entry, &mut file)?;
        file.sync_all()?;
        files.insert(name, destination);
    }
    let manifest_path = files
        .get(crate::release::RELEASE_MANIFEST_NAME)
        .context("BAD_REQUEST: native bundle has no gridstator-release.json")?;
    let bytes = fs::read(manifest_path)?;
    let manifest = ReleaseManifest::from_bytes(&bytes)?;
    if let Some(active) = active {
        ensure_active_manifest(&manifest, active)?;
    }
    ensure!(
        files.len() == 5,
        "BAD_REQUEST: native bundle must contain all four target archives"
    );
    for artifact in manifest.artifacts.values() {
        ensure!(
            files.contains_key(&artifact.archive.file),
            "BAD_REQUEST: native bundle is missing a target archive"
        );
    }
    let mirror = mirror(directory);
    mirror.cache_manifest(&bytes, &crate::release::sha256_bytes(&bytes))?;
    for (target, artifact) in &manifest.artifacts {
        mirror.cache_artifact(&manifest, target, &files[&artifact.archive.file])?;
    }
    Ok(manifest)
}
pub async fn publish_path(
    controller: Arc<Controller>,
    path: &Path,
    sha256: &str,
    principal: Value,
) -> Result<Response> {
    admin(&principal)?;
    let store = managed_store_background(Some(controller.clone())).await?;
    // Keep an open descriptor before queuing blocking work: cancelling the HTTP handler can
    // unlink its temporary pathname without invalidating an import that has already started.
    let source = fs::File::open(path)?;
    let expected = sha256.to_owned();
    let directory = controller.dir.clone();
    let owner = controller.clone();
    let manifest = tokio::task::spawn_blocking(move || {
        let _owner = owner;
        let active = store.manifest(crate::BUILD_ID)?;
        import_bundle_file_with_active(source, &expected, &directory, Some(&active))
    })
    .await??;
    let response = schedule_controller(controller, &manifest).await?;
    Ok(Json(response).into_response())
}
pub async fn publish(
    controller: Arc<Controller>,
    bytes: Vec<u8>,
    sha256: &str,
    principal: Value,
) -> Result<Response> {
    ensure!(
        bytes.len() as u64 <= MAX_BUNDLE_BYTES,
        "BAD_REQUEST: native bundle exceeds size limit"
    );
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    publish_path(controller, file.path(), sha256, principal).await
}

// Only network futures may be cancelled here. Started blocking installation/hash/state work must
// retain its owners and be joined, even when worker shutdown has already been requested.
async fn update_network<T, E>(
    cancel: &tokio_util::sync::CancellationToken,
    operation: impl std::future::Future<Output = std::result::Result<T, E>>,
) -> Result<T>
where
    E: Into<anyhow::Error>,
{
    tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("Managed update cancelled during shutdown"),
        result = operation => result.map_err(Into::into),
    }
}

pub async fn worker_update(
    check: crate::worker::UpdateCheck,
) -> Result<crate::worker::UpdateDecision> {
    use crate::worker::UpdateDecision;
    if check.active_jobs > 0 || check.pending_results > 0 {
        return Ok(UpdateDecision::None);
    }
    let store = match managed_store_background(None).await {
        Ok(store) => store,
        Err(_) => return Ok(UpdateDecision::None),
    };
    let tool_changed = match act::sync_worker(&check).await {
        Ok(changed) => changed,
        Err(error) => {
            let _ = update_network(&check.cancel, check.client.post("/v2/agent/update/report",json!({"target":crate::BUILD_ID,"error":format!("{error:#}").chars().take(4096).collect::<String>()}))).await;
            return Err(error);
        }
    };
    if check.embedded {
        return Ok(if tool_changed {
            UpdateDecision::Reprobe
        } else {
            UpdateDecision::None
        });
    }
    let target = crate::release::host_target()?;
    let query = format!(
        "/v2/agent/update?current={}&target={target}&mode={}",
        crate::BUILD_ID,
        if check.update_only {
            "update-only"
        } else {
            "normal"
        }
    );
    let response = update_network(&check.cancel, check.client.get(&query)).await?;
    if response["action"] == "none" {
        return Ok(if tool_changed {
            UpdateDecision::Reprobe
        } else {
            UpdateDecision::None
        });
    }
    ensure!(
        response["action"] == "update",
        "Unsupported native update response"
    );
    let build = response["buildId"]
        .as_str()
        .context("native update buildId required")?
        .to_owned();
    let selected = build.clone();
    let result = async {
        let manifest: ReleaseManifest = serde_json::from_value(response["manifest"].clone())?;
        manifest.validate()?;
        ensure!(
            manifest.build_id == build && response["manifestSha256"] == manifest.sha256()?,
            "Native update manifest identity/digest mismatch"
        );
        let artifact = manifest.target(target)?.clone();
        ensure!(
            response["artifact"]["sha256"] == artifact.archive.sha256
                && response["artifact"]["size"].as_u64() == Some(artifact.archive.size),
            "Native artifact differs from its manifest"
        );
        let path = response["artifact"]["path"]
            .as_str()
            .context("native artifact path required")?;
        ensure!(
            path.starts_with("/v2/agent/update/artifact?") && !path.contains(['\r', '\n']),
            "Native update artifact must use the pinned controller path"
        );
        let directory = check.dir.join("updates");
        state::private_dir(&directory)?;
        let temporary = tempfile::NamedTempFile::new_in(directory)?;
        let file = tokio::fs::File::from_std(temporary.reopen()?);
        let mut file = file;
        let mut incoming = update_network(
            &check.cancel,
            check
                .client
                .http
                .get(format!("{}{}", check.client.config.url, path))
                .bearer_auth(&check.client.config.token)
                .send(),
        )
        .await?
        .error_for_status()?;
        ensure!(
            incoming
                .content_length()
                .is_none_or(|length| length == artifact.archive.size),
            "Native artifact response has unexpected length"
        );
        use sha2::Digest;
        use tokio::io::AsyncWriteExt;
        let mut hash = sha2::Sha256::new();
        let mut received = 0u64;
        while let Some(chunk) = update_network(&check.cancel, incoming.chunk()).await? {
            received += chunk.len() as u64;
            ensure!(
                received <= artifact.archive.size,
                "Native artifact response exceeds announced size"
            );
            hash.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        drop(file);
        ensure!(
            received == artifact.archive.size
                && hex::encode(hash.finalize()) == artifact.archive.sha256,
            "Native artifact size/SHA-256 mismatch"
        );
        let timeout = response["confirmWithinSeconds"]
            .as_u64()
            .context("native confirmation timeout required")?;
        ensure!(
            (1..=86400).contains(&timeout),
            "Native confirmation timeout out of range"
        );
        let path = temporary.path().to_owned();
        let target = target.to_owned();
        ensure!(
            !check.cancel.is_cancelled(),
            "Worker update cancelled before installation"
        );
        tokio::task::spawn_blocking(move || {
            // Retain the download until all verification, staging and activation have finished.
            let _temporary = temporary;
            store.install_verified(&manifest, &target, &path)?;
            store.activate(&manifest.build_id, Some(Duration::from_secs(timeout)))?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok::<_, anyhow::Error>(UpdateDecision::Restart)
    }
    .await;
    if let Err(error) = &result {
        let _ = update_network(&check.cancel, check.client.post("/v2/agent/update/report",json!({"target":selected,"error":format!("{error:#}").chars().take(4096).collect::<String>()}))).await;
    }
    result
}
/// The controller runs this in a dedicated periodic task; network preparation never holds its DB
/// owner or delays the independent scheduler/heartbeat loops.
pub async fn tick(controller: Arc<Controller>) -> Result<()> {
    tick_with_cancel(controller, tokio_util::sync::CancellationToken::new()).await
}
pub async fn tick_with_cancel(
    controller: Arc<Controller>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Ok(());
    }
    ensure_current_mirror_with_cancel(controller.clone(), cancel.clone()).await?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    act::tick_with_cancel(controller.clone(), cancel.clone()).await?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    coordinate_workers(controller.clone()).await?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    let result = advance_controller(controller.clone()).await;
    if let Err(error) = &result {
        let message = format!("{error:#}");
        let activated = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
            .and_then(|root| {
                InstallStore::new(PathBuf::from(root))
                    .state()
                    .ok()
                    .flatten()
            })
            .is_some_and(|state| state.current != crate::BUILD_ID);
        if activated {
            controller.request_restart();
        } else {
            controller
                .db
                .write(move |connection| {
                    if let Some(mut pending) = db::get(connection, "updates", "controller")?
                        && pending["phase"] == "launching"
                    {
                        if let Some(id) = pending["embeddedNode"].as_str()
                            && let Some(mut node) = db::get(connection, "nodes", id)?
                            && node["settings"]["schedulingMode"] == "draining"
                        {
                            node["settings"]["schedulingMode"] = pending["previousMode"].clone();
                            db::put(connection, "nodes", id, &node)?;
                        }
                        pending["phase"] = json!("failed");
                        pending["error"] = json!(message);
                        db::put(connection, "updates", "controller", &pending)?;
                    }
                    Ok(())
                })
                .await?;
        }
    }
    result
}
async fn ensure_current_mirror(controller: Arc<Controller>) -> Result<()> {
    ensure_current_mirror_with_cancel(controller, tokio_util::sync::CancellationToken::new()).await
}
async fn ensure_current_mirror_with_cancel(
    controller: Arc<Controller>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Ok(());
    }
    let store = match managed_store_background(Some(controller.clone())).await {
        Ok(store) => store,
        Err(_) => return Ok(()),
    };
    let directory = controller.dir.clone();
    if tokio::task::spawn_blocking(move || target_view(&directory).1).await? {
        return Ok(());
    }
    let now = state::now();
    let pid = std::process::id();
    let claimed=controller.db.write(move|connection|{let prior=db::get(connection,"updates","mirrorPreparation")?.unwrap_or(Value::Null);let same=prior["target"]==crate::BUILD_ID;let live=same&&prior["pid"]==pid&&prior["phase"]=="preparing"&&now-prior["at"].as_i64().unwrap_or(0)<15*60*1000;let backoff=same&&prior["retryAt"].as_i64().is_some_and(|retry|retry>now);if live||backoff{return Ok(false);}db::put(connection,"updates","mirrorPreparation",&json!({"target":crate::BUILD_ID,"pid":pid,"at":now,"phase":"preparing","error":null}))?;Ok(true)}).await?;
    if !claimed {
        return Ok(());
    }
    let result=async{let source=controller.releases.clone().with_cancellation(cancel).with_lifetime_guard(controller.clone());let release=source.fetch_manifest(&format!("v{}",crate::VERSION)).await?;ensure!(release.manifest.build_id==crate::BUILD_ID,"Official version tag does not describe this exact native build; publish its verified native bundle to populate the controller mirror");let selected=release.manifest.clone();let owner=controller.clone();tokio::task::spawn_blocking(move||{let _owner=owner;ensure_active_manifest(&selected,&store.manifest(crate::BUILD_ID)?)}).await??;let mirror=mirror(&controller.dir);for target in crate::release::SUPPORTED_TARGETS{source.mirror_artifact(&release,target,&mirror).await?;}Ok::<(),anyhow::Error>(())}.await;
    let error = result.as_ref().err().map(|error| format!("{error:#}"));
    let failed = error.is_some();
    controller.db.write(move|connection|db::put(connection,"updates","mirrorPreparation",&json!({"target":crate::BUILD_ID,"pid":pid,"at":now,"phase":if failed{"failed"}else{"ready"},"error":error,"retryAt":if failed{Some(state::now()+60000)}else{None}}))).await?;
    if let Err(error) = result {
        eprintln!("Native release mirror preparation: {error:#}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn queued_bundle_import_owns_its_source_after_handler_unlinks_the_upload() -> Result<()> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let archive = tar::Builder::new(encoder).into_inner()?.finish()?;
        let expected = crate::release::sha256_bytes(&archive);
        let mut upload = tempfile::NamedTempFile::new()?;
        upload.write_all(&archive)?;
        let source = upload.reopen()?;
        let path = upload.path().to_owned();
        drop(upload);
        ensure!(!path.exists(), "upload pathname still exists");
        let mirror = tempfile::tempdir()?;
        // An empty archive reaches manifest validation through the owned descriptor even when
        // the HTTP upload guard has already removed its pathname.
        let error = import_bundle_file(source, &expected, mirror.path()).unwrap_err();
        ensure!(
            error.to_string().contains("no gridstator-release.json"),
            "lost owned upload descriptor: {error:#}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_interrupts_stalled_update_headers_and_bodies() -> Result<()> {
        for send_headers in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let (ready, started) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await?;
                let mut request = [0; 4096];
                let received = stream.read(&mut request).await?;
                ensure!(
                    received > 0,
                    "update test connection closed before the request"
                );
                if send_headers {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\na")
                        .await?;
                }
                let _ = ready.send(());
                std::future::pending::<()>().await;
                Ok::<_, anyhow::Error>(())
            });
            let cancel = tokio_util::sync::CancellationToken::new();
            let shutdown = cancel.clone();
            let (body_ready, body_started) = tokio::sync::oneshot::channel();
            let request = tokio::spawn(async move {
                let client = reqwest::Client::new();
                let mut response =
                    update_network(&cancel, client.get(format!("http://{address}")).send()).await?;
                let mut body_ready = Some(body_ready);
                while update_network(&cancel, response.chunk()).await?.is_some() {
                    if let Some(ready) = body_ready.take() {
                        let _ = ready.send(());
                    }
                }
                Ok::<_, anyhow::Error>(())
            });
            started.await?;
            if send_headers {
                tokio::time::timeout(Duration::from_secs(1), body_started).await??;
            }
            shutdown.cancel();
            // The server remains connected and never completes its response. Both an initial
            // metadata/header stall and an artifact body stall must release on worker shutdown.
            let result = tokio::time::timeout(Duration::from_secs(1), request).await;
            server.abort();
            let error = result
                .context("shutdown retained a stalled update request")??
                .unwrap_err();
            ensure!(
                error.to_string().contains("cancelled"),
                "unexpected request failure: {error:#}"
            );
        }
        Ok(())
    }
}
