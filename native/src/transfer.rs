//! Streaming content-addressed transfers with bounded pipe buffers and digest verification.
use crate::{
    client::Client,
    workspace::{self, BlobStore, Capture, UnpackLimits},
};
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{Value, json};
use std::{io, path::Path, sync::Arc};
use tokio_util::io::{ReaderStream, StreamReader, SyncIoBridge};
use tokio_util::sync::CancellationToken;

struct CancelTransfer(CancellationToken);
impl Drop for CancelTransfer {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub async fn upload_capture(client: &Client, captured: Arc<Capture>) -> Result<()> {
    upload_capture_with_summary(client, captured)
        .await
        .map(|_| ())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct UploadSummary {
    pub uploaded_blobs: usize,
    pub uploaded_bytes: u64,
}

/// Report verified source bytes sent by the existing missing-blob transfer, without
/// another capture or controller query. Tar framing is excluded, matching v1.
pub async fn upload_capture_with_summary(
    client: &Client,
    captured: Arc<Capture>,
) -> Result<UploadSummary> {
    let mut digests = captured.blobs.keys().cloned().collect::<Vec<_>>();
    digests.push(captured.id.clone());
    let mut missing = vec![];
    for chunk in digests.chunks(10000) {
        let response = client
            .post("/v1/blobs/missing", json!({"digests":chunk}))
            .await?;
        missing.extend(
            response["missing"]
                .as_array()
                .context("missing blob response")?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    let mut summary = UploadSummary::default();
    for chunk in missing.chunks(32) {
        let bytes = chunk
            .iter()
            .try_fold(0u64, |bytes, digest| -> Result<u64> {
                let size = if digest == &captured.id {
                    captured.manifest_bytes.len() as u64
                } else {
                    captured
                        .blobs
                        .get(digest)
                        .context("controller requested an unknown capture blob")?
                        .size
                };
                bytes
                    .checked_add(size)
                    .context("capture upload byte count overflow")
            })?;
        let capture = captured.clone();
        let digests = chunk.to_vec();
        let (writer, reader) = tokio::io::duplex(64 * 1024);
        let writer = SyncIoBridge::new(writer);
        let pack = tokio::task::spawn_blocking(move || {
            workspace::pack_capture_blobs(&capture, &digests, writer)
        });
        let response = client
            .http
            .post(format!("{}/v1/blobs/upload", client.config.url))
            .bearer_auth(&client.config.token)
            .header("content-type", "application/x-tar")
            .body(reqwest::Body::wrap_stream(ReaderStream::new(reader)))
            .send()
            .await;
        match response {
            Ok(response) => {
                response.error_for_status()?;
                pack.await??;
                summary.uploaded_blobs += chunk.len();
                summary.uploaded_bytes = summary
                    .uploaded_bytes
                    .checked_add(bytes)
                    .context("capture upload byte count overflow")?;
            }
            Err(error) => {
                let _ = pack.await;
                return Err(error.into());
            }
        }
    }
    client
        .post("/v1/snapshots", json!({"manifestDigest":captured.id}))
        .await?;
    Ok(summary)
}
pub async fn download_blobs(client: &Client, store: &BlobStore, digests: &[String]) -> Result<()> {
    for chunk in digests.chunks(32) {
        let response = client
            .http
            .post(format!("{}/v1/blobs/download", client.config.url))
            .bearer_auth(&client.config.token)
            .json(&json!({"digests":chunk}))
            .send()
            .await?
            .error_for_status()?;
        // spawn_blocking is not aborted when its JoinHandle is dropped. Closing this stream
        // on caller cancellation also wakes the blocking tar reader and permits runtime shutdown.
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = CancelTransfer(cancellation.clone());
        let stream = response
            .bytes_stream()
            .take_until(cancellation.cancelled_owned())
            .map_err(io::Error::other)
            .boxed();
        let reader = SyncIoBridge::new(StreamReader::new(stream));
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            workspace::unpack_blobs(
                reader,
                &store,
                UnpackLimits {
                    max_entries: Some(1000000),
                    max_bytes: Some(8 * 1024 * 1024 * 1024),
                },
            )
        })
        .await??;
    }
    Ok(())
}
pub async fn upload_artifact(client: &Client, path: &Path, name: &str) -> Result<Value> {
    ensure!(
        workspace::check_snapshot_path(name).is_ok(),
        "artifact name must be a safe relative path"
    );
    let file = path.to_owned();
    let (size, hash) =
        tokio::task::spawn_blocking(move || crate::release::hash_file(file)).await??;
    let digest = format!("sha256:{hash}");
    let missing = client
        .post("/v1/blobs/missing", json!({"digests":[digest]}))
        .await?;
    if missing["missing"].as_array().is_some_and(|a| !a.is_empty()) {
        let (writer, reader) = tokio::io::duplex(64 * 1024);
        let writer = SyncIoBridge::new(writer);
        let path = path.to_owned();
        let task = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut tar = tar::Builder::new(writer);
            let mut header = tar::Header::new_gnu();
            header.set_size(size);
            header.set_mode(0o600);
            header.set_cksum();
            tar.append_data(
                &mut header,
                format!("sha256/{hash}"),
                std::fs::File::open(path)?,
            )?;
            tar.finish()?;
            Ok(())
        });
        let result = client
            .http
            .post(format!("{}/v1/blobs/upload", client.config.url))
            .bearer_auth(&client.config.token)
            .header("content-type", "application/x-tar")
            .body(reqwest::Body::wrap_stream(ReaderStream::new(reader)))
            .send()
            .await;
        match result {
            Ok(response) => {
                response.error_for_status()?;
                task.await??;
            }
            Err(error) => {
                let _ = task.await;
                return Err(error.into());
            }
        }
    }
    Ok(json!({"name":name,"digest":digest,"size":size}))
}
