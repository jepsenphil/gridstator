use anyhow::{Context, Result};
use gridstator::{
    controller::{self, Controller, ControllerTiming},
    db, protocol, state,
    workspace::{BlobStore, FileEntry, Manifest, ManifestEntry, digest_of},
};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;
async fn controller() -> Result<(TempDir, std::sync::Arc<Controller>)> {
    let temp = TempDir::new()?;
    let dir = temp.path().join("controller");
    controller::initialize(
        &dir,
        "127.0.0.1",
        0,
        &[],
        false,
        &temp.path().join("config.json"),
    )
    .await?;
    let timing = ControllerTiming {
        startup_grace_ms: 1000,
        ..ControllerTiming::default()
    };
    Ok((temp, Controller::open_with_timing(dir, timing).await?))
}
fn blob(store: &BlobStore, bytes: &[u8], old: bool) -> Result<String> {
    let id = digest_of(bytes);
    store.put_bytes(&id, bytes)?;
    if old {
        std::fs::File::open(store.path(&id)?)?.set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::now() - Duration::from_secs(7 * 3600)),
        )?;
    }
    Ok(id)
}
fn snapshot(store: &BlobStore, name: &str, body: &str) -> Result<(String, Value)> {
    let digest = blob(store, body.as_bytes(), true)?;
    let manifest = Manifest {
        version: 1,
        entries: vec![ManifestEntry::File(FileEntry {
            path: name.into(),
            digest,
            size: body.len() as u64,
            executable: false,
        })],
    };
    let id = blob(store, &manifest.serialize()?, true)?;
    Ok((id, serde_json::to_value(manifest)?))
}
#[tokio::test]
async fn startup_grace_preserves_inflight_then_requeues_assigned_and_retries_lost() -> Result<()> {
    let (_temp, controller) = controller().await?;
    let now = state::now();
    let (assigned, running) = controller
        .db
        .write(move |c| {
            let spec = protocol::normalize_spec(
                json!({"kind":"command","command":["echo","test"],"retry":{"max":1,"on":["lost"]}}),
            )?;
            let mut ids = vec![];
            for phase in ["assigned", "running"] {
                let mut job =
                    controller::create_job(c, spec.clone(), "original-owner", None, None)?;
                for (key, value) in [
                    ("state", json!(phase)),
                    ("nodeId", json!("offline-node")),
                    ("nodeName", json!("offline")),
                    ("assignmentId", json!("old-assignment")),
                    ("assignedAt", json!(now - 100_000)),
                ] {
                    job[key] = value;
                }
                let id = job["id"].as_str().unwrap().to_owned();
                db::put(c, "jobs", &id, &job)?;
                ids.push(id);
            }
            Ok((ids.remove(0), ids.remove(0)))
        })
        .await?;
    controller.sweep_at(now + 500).await?;
    let id = running.clone();
    assert_eq!(
        controller
            .db
            .call(move |c| db::get(c, "jobs", &id))
            .await?
            .unwrap()["state"],
        "running"
    );
    controller.sweep_at(now + 2000).await?;
    let jobs = controller.db.call(|c| db::list(c, "jobs")).await?;
    let queued = jobs.iter().find(|job| job["id"] == assigned).unwrap();
    assert_eq!(queued["state"], "queued");
    for key in ["nodeId", "nodeName", "assignmentId", "assignedAt"] {
        assert!(queued[key].is_null());
    }
    let lost = jobs.iter().find(|job| job["id"] == running).unwrap();
    assert_eq!(lost["state"], "lost");
    let retry = jobs
        .iter()
        .find(|job| job["retryOf"] == running)
        .context("lost retry")?;
    assert_eq!(retry["owner"], "original-owner");
    assert_eq!(retry["retriesLeft"], 0);
    Ok(())
}
#[tokio::test]
async fn retention_keeps_active_inputs_and_artifacts_graces_uploads_and_sweeps_old_orphans()
-> Result<()> {
    let (_temp, controller) = controller().await?;
    let store = BlobStore::new(controller.dir.join("blobs"));
    let now = state::now();
    let old = now - 20 * 86_400_000;
    let (active_id, active_manifest) = snapshot(&store, "active.txt", "active input")?;
    let (stale_id, stale_manifest) = snapshot(&store, "stale.txt", "stale input")?;
    let artifact = blob(&store, b"retained artifact", true)?;
    let orphan = blob(&store, b"old orphan", true)?;
    let fresh = blob(&store, b"new upload", false)?;
    let active_copy = active_id.clone();
    let stale_copy = stale_id.clone();
    let artifact_copy = artifact.clone();
    let(deleted_id,retained_id)=controller.db.write(move|c|{db::put(c,"settings","cluster",&json!({"jobRetentionDays":10,"logRetentionDays":1,"snapshotRetentionDays":1,"autoUpdate":"off","updateWindow":null,"toolchain":{"act":"off"}}))?;
        for(id,manifest)in[(&active_copy,active_manifest),(&stale_copy,stale_manifest)]{db::put(c,"snapshots",id,&json!({"id":id,"manifest":manifest,"createdAt":old,"lastUsedAt":old,"owner":"source","fileCount":1,"totalBytes":12}))?;}
        let spec=protocol::normalize_spec(json!({"kind":"command","command":["echo","test"]}))?;
        let mut deleted=controller::create_job(c,spec.clone(),"source",Some(("empty-group",1,1)),None)?;deleted["state"]=json!("succeeded");deleted["finishedAt"]=json!(old);let deleted_id=deleted["id"].as_str().unwrap().to_owned();db::put(c,"groups","empty-group",&json!({"id":"empty-group"}))?;db::put(c,"jobs",&deleted_id,&deleted)?;
        let mut retained=controller::create_job(c,spec.clone(),"source",None,None)?;retained["state"]=json!("failed");retained["finishedAt"]=json!(now-2*86_400_000);retained["artifacts"]=json!([{"name":"report","digest":artifact_copy,"size":17}]);let retained_id=retained["id"].as_str().unwrap().to_owned();db::put(c,"jobs",&retained_id,&retained)?;c.execute("INSERT INTO logs(job_id,seq,data)VALUES(?1,1,'{}')",[&retained_id])?;
        let mut active=controller::create_job(c,spec,"source",None,None)?;active["snapshotId"]=json!(active_copy);active["spec"]["snapshot"]=active["snapshotId"].clone();active["state"]=json!("running");db::put(c,"jobs",active["id"].as_str().unwrap(),&active)?;Ok((deleted_id,retained_id))
    }).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = controller.dir.join("unrelated");
        std::fs::write(&outside, b"do not remove")?;
        let link = store.path(&format!("sha256:{}", "0".repeat(64)))?;
        std::fs::create_dir_all(link.parent().unwrap())?;
        symlink(&outside, &link)?;
    }
    let summary = controller.run_retention_at(now).await?;
    assert_eq!(summary["jobsDeleted"], 1);
    assert_eq!(summary["logsDeleted"], 1);
    assert_eq!(summary["snapshotsDeleted"], 1);
    assert_eq!(summary["blobsDeleted"], 3);
    assert!(!store.has(&orphan)?);
    assert!(!store.has(&stale_id)?);
    assert!(store.has(&active_id)?);
    assert!(store.has(&artifact)?);
    assert!(store.has(&fresh)?);
    let active = active_id.clone();
    assert!(
        controller
            .db
            .call(move |c| db::get(c, "snapshots", &active))
            .await?
            .is_some()
    );
    let stale = stale_id.clone();
    assert!(
        controller
            .db
            .call(move |c| db::get(c, "snapshots", &stale))
            .await?
            .is_none()
    );
    assert!(
        controller
            .db
            .call(move |c| db::get(c, "jobs", &deleted_id))
            .await?
            .is_none()
    );
    assert!(
        controller
            .db
            .call(move |c| db::logs(c, &retained_id, 0, 100))
            .await?
            .is_empty()
    );
    assert!(
        controller
            .db
            .call(|c| db::get(c, "groups", "empty-group"))
            .await?
            .is_none()
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::read(controller.dir.join("unrelated"))?,
        b"do not remove"
    );
    Ok(())
}
#[tokio::test]
async fn unreadable_snapshot_is_removed_without_following_cas_symlinks() -> Result<()> {
    let (_temp, controller) = controller().await?;
    let id = digest_of(b"missing manifest");
    let selected = id.clone();
    controller
        .db
        .write(move |c| {
            db::put(
                c,
                "snapshots",
                &selected,
                &json!({"id":selected,"createdAt":state::now(),"lastUsedAt":state::now()}),
            )
        })
        .await?;
    let result = controller.run_retention_at(state::now()).await?;
    assert_eq!(result["snapshotsDeleted"], 1);
    assert!(
        controller
            .db
            .call(move |c| db::get(c, "snapshots", &id))
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn initialize_brackets_ipv6_listener_urls() -> Result<()> {
    let temp = TempDir::new()?;
    let init = controller::initialize(
        &temp.path().join("controller"),
        "::1",
        7300,
        &[],
        false,
        &temp.path().join("client.json"),
    )
    .await?;
    assert_eq!(init["url"], "https://[::1]:7300");
    assert_eq!(
        gridstator::client::normalize_url(init["url"].as_str().unwrap())?,
        "https://[::1]:7300"
    );
    Ok(())
}
