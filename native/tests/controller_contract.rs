use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use gridstator::{
    client::{Client, Config},
    controller, pki,
};
use serde_json::{Value, json};
use std::time::Duration;
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

async fn boot() -> Result<(TempDir, tokio::task::JoinHandle<Result<()>>, Client)> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = TempDir::new()?;
    let dir = temp.path().join("controller");
    let init = controller::initialize(
        &dir,
        "127.0.0.1",
        0,
        &[],
        false,
        &temp.path().join("client.json"),
    )
    .await?;
    let server = tokio::spawn(controller::start(dir.clone(), None, None));
    let runtime = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(runtime) = gridstator::state::read_json(&dir.join("runtime.json")) {
                break runtime;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let client = Client::new(Config {
        version: 1,
        url: format!("https://127.0.0.1:{}", runtime["port"].as_u64().unwrap()),
        ca_pem: std::fs::read_to_string(dir.join("pki/ca.pem"))?,
        ca_fingerprint: init["caFingerprint"].as_str().unwrap().into(),
        token: init["token"].as_str().unwrap().into(),
        token_name: Some("local-admin".into()),
    })?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client.get("/v1/health").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    Ok((temp, server, client))
}

#[tokio::test]
async fn source_controller_publish_checksum_error_reports_expected_and_actual() -> Result<()> {
    let (_temp, server, client) = boot().await?;
    let expected = "0".repeat(64);
    let bytes = b"not-a-native-bundle";
    let actual = gridstator::workspace::digest_of(bytes);
    let response = client
        .http
        .post(format!(
            "{}/v1/controller/build?sha256={expected}",
            client.config.url
        ))
        .bearer_auth(&client.config.token)
        .body(bytes.as_slice())
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let error: Value = response.json().await?;
    let message = error["error"]["message"]
        .as_str()
        .context("checksum error message")?;
    assert!(
        message.contains(&format!("expected {expected}")),
        "{message}"
    );
    assert!(
        message.contains(actual.strip_prefix("sha256:").unwrap()),
        "{message}"
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn retained_sdk_blob_get_decodes_digest_once_and_returns_exact_bytes() -> Result<()> {
    let (temp, server, client) = boot().await?;
    let bytes = b"exact blob\0\xff\xe2\x82\xac";
    let digest = gridstator::workspace::digest_of(bytes);
    let store = gridstator::workspace::BlobStore::new(temp.path().join("controller/blobs"));
    store.init()?;
    store.put_bytes(&digest, bytes)?;
    let encoded: String = url::form_urlencoded::byte_serialize(digest.as_bytes()).collect();
    assert!(encoded.starts_with("sha256%3A"));
    for path in [&digest, &encoded, &encoded.replace("%3A", "%3a")] {
        let response = client
            .http
            .get(format!("{}/v1/blobs/{path}", client.config.url))
            .bearer_auth(&client.config.token)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "retained SDK path {path}"
        );
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        assert_eq!(response.bytes().await?.as_ref(), bytes);
    }
    for path in [
        encoded.replace("%3A", "%253A"),
        format!("{encoded}%2F..%2Fcontroller.json"),
    ] {
        let response = client
            .http
            .get(format!("{}/v1/blobs/{path}", client.config.url))
            .bearer_auth(&client.config.token)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "unsafe/double-decoded path {path}"
        );
    }
    let response = client
        .http
        .get(format!("{}/v1/blobs/sha256%ZZ", client.config.url))
        .bearer_auth(&client.config.token)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn native_tls_pairing_authority_and_job_lifecycle() -> Result<()> {
    let (_temp, server, client) = boot().await?;
    let ca = pki::fetch_pinned_ca(&client.config.url, &client.config.ca_fingerprint).await?;
    assert_eq!(ca, client.config.ca_pem);
    assert!(
        pki::fetch_pinned_ca(&client.config.url, &format!("sha256:{}", "0".repeat(64)))
            .await
            .is_err()
    );
    let mut invalid = client.config.clone();
    invalid.token = "gsc_invalid".into();
    let unauthorized = Client::new(invalid)?;
    assert_eq!(unauthorized.get("/v1/health").await?["ok"], true);
    assert!(
        unauthorized
            .get("/v1/jobs")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("401")
    );
    assert!(
        unauthorized
            .get("/v1/cluster")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("401")
    );
    let operator = client
        .post("/v1/tokens", json!({"name":"operator","role":"operator"}))
        .await?;
    let operator_id = operator["info"]["id"]
        .as_str()
        .context("operator token id")?
        .to_owned();
    let mut config = client.config.clone();
    config.token = operator["token"].as_str().unwrap().into();
    let operator = Client::new(config)?;
    assert!(
        operator.get("/v1/jobs").await?["jobs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        operator
            .post("/v1/tokens", json!({"kind":"client","name":"forbidden"}))
            .await
            .unwrap_err()
            .to_string()
            .starts_with("403")
    );
    assert!(
        operator
            .get("/v1/tokens")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("403")
    );
    let join = client
        .post("/v1/tokens", json!({"kind":"join","name":"join"}))
        .await?;
    let mut config = client.config.clone();
    config.token = join["token"].as_str().unwrap().into();
    let pairing = Client::new(config)?;
    assert!(
        pairing
            .get("/v1/jobs")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("403")
    );
    let info = json!({"hostname":"scripted-worker","platform":"linux","arch":"x64","osRelease":"test","cpus":8,"memoryTotalMb":16384,"agentVersion":gridstator::VERSION,"docker":{"available":true,"version":"test"},"buildx":{"available":false},"act":{"available":false},"executors":["command"]});
    let registered = pairing
        .post(
            "/v1/agents/register",
            json!({"name":"scripted","info":info}),
        )
        .await?;
    assert!(
        registered["credential"]
            .as_str()
            .unwrap()
            .starts_with("gsa_")
    );
    assert!(
        pairing
            .post("/v1/agents/register", json!({"name":"second","info":info}))
            .await
            .unwrap_err()
            .to_string()
            .starts_with("401")
    );
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
        Some(tokio_tungstenite::Connector::Rustls(pki::websocket_tls(
            &ca,
        )?)),
    )
    .await?;
    socket
        .send(Message::Text(
            json!({"type":"hello","protocolVersion":1,"info":info,"activeJobs":[],"results":[]})
                .to_string()
                .into(),
        ))
        .await?;
    let welcome = socket.next().await.context("welcome")??;
    assert!(welcome.to_text()?.contains("welcome"));
    let job=operator.post("/v1/jobs",json!({"spec":{"kind":"command","command":["echo","native"],"secretEnv":{"TOKEN":"private-value"}}})).await?;
    assert_eq!(job["spec"]["secretEnv"]["TOKEN"], "***");
    assert!(job.get("retriesLeft").is_none());
    let assignment = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let msg = socket.next().await.context("assignment")??;
            let value: Value = serde_json::from_str(msg.to_text()?)?;
            if value["type"] == "job.assign" {
                break Ok::<Value, anyhow::Error>(value);
            }
        }
    })
    .await??;
    assert_eq!(assignment["spec"]["secretEnv"]["TOKEN"], "private-value");
    let result = json!({"jobId":job["id"],"assignmentId":assignment["assignmentId"],"outcome":"succeeded","exitCode":0,"error":null,"oomKilled":false,"finishedAt":gridstator::state::now(),"logSeq":1,"outputs":null,"artifacts":[]});
    // A terminal result must not overtake its durable log spool.
    socket
        .send(Message::Text(
            json!({"type":"job.result","result":result})
                .to_string()
                .into(),
        ))
        .await?;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(!gridstator::protocol::terminal(
        client
            .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
            .await?["state"]
            .as_str()
            .unwrap()
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(40), socket.next())
            .await
            .is_err(),
        "terminal result was acknowledged before its missing durable log sequence"
    );
    socket.send(Message::Text(json!({"type":"job.log","jobId":job["id"],"assignmentId":assignment["assignmentId"],"entries":[{"s":1,"t":gridstator::state::now(),"o":"stdout","d":"native 🦀\n"}]}).to_string().into())).await?;
    socket.send(Message::Text(json!({"type":"job.log","jobId":job["id"],"assignmentId":assignment["assignmentId"],"entries":[{"s":1,"t":gridstator::state::now(),"o":"stdout","d":"duplicate must not replace the original\n"}]}).to_string().into())).await?;
    socket
        .send(Message::Text(
            json!({"type":"job.result","result":result})
                .to_string()
                .into(),
        ))
        .await?;
    let ack = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await?
        .context("ack")??;
    assert!(ack.to_text()?.contains("job.ack"));
    assert!(
        tokio::time::timeout(Duration::from_millis(40), socket.next())
            .await
            .is_err(),
        "one complete result emitted multiple acknowledgements"
    );
    let completed = client
        .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
        .await?;
    assert_eq!(completed["state"], "succeeded");
    let logs = client
        .get(&format!("/v1/jobs/{}/logs", job["id"].as_str().unwrap()))
        .await?;
    assert_eq!(logs["entries"][0]["d"], "native 🦀\n");
    assert_eq!(logs["entries"].as_array().unwrap().len(), 1);
    assert_eq!(logs["complete"], true);
    let retried = operator
        .post(
            &format!("/v1/jobs/{}/retry", job["id"].as_str().unwrap()),
            json!({}),
        )
        .await?;
    assert_ne!(retried["id"], job["id"]);
    assert_eq!(retried["retryOf"], job["id"]);
    assert_eq!(retried["attempt"], 2);
    client
        .request(
            reqwest::Method::DELETE,
            &format!("/v1/tokens/{operator_id}"),
            None,
        )
        .await?;
    assert!(
        operator
            .get("/v1/jobs")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("401")
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn native_blob_upload_checks_content_and_snapshot_manifest() -> Result<()> {
    let (_temp, server, client) = boot().await?;
    let bytes = b"exact uncommitted content";
    let digest = gridstator::workspace::digest_of(bytes);
    let mut archive = tar::Builder::new(vec![]);
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    archive.append_data(
        &mut header,
        format!("sha256/{}", digest.trim_start_matches("sha256:")),
        bytes.as_slice(),
    )?;
    let tar = archive.into_inner()?;
    let upload = client
        .http
        .post(format!("{}/v1/blobs/upload", client.config.url))
        .bearer_auth(&client.config.token)
        .header("content-type", "application/x-tar")
        .body(tar)
        .send()
        .await?;
    assert!(upload.status().is_success());
    let missing = client
        .post("/v1/blobs/missing", json!({"digests":[digest]}))
        .await?;
    assert!(missing["missing"].as_array().unwrap().is_empty());
    let manifest = json!({"version":1,"entries":[{"path":"source.txt","type":"file","size":bytes.len(),"digest":digest,"executable":false}]});
    let snapshot = client
        .post("/v1/snapshots", json!({"manifest":manifest}))
        .await?;
    assert!(gridstator::protocol::valid_digest(
        snapshot["id"].as_str().unwrap()
    ));
    let id = snapshot["id"].as_str().unwrap();
    assert!(snapshot.get("manifest").is_none());
    let repeated = client
        .post("/v1/snapshots", json!({"manifestDigest":id}))
        .await?;
    assert_eq!(repeated["id"], snapshot["id"]);
    assert_eq!(repeated["createdAt"], snapshot["createdAt"]);
    assert_eq!(repeated["fileCount"], 1);
    assert!(
        client
            .get(&format!("/v1/snapshots/{id}"))
            .await?
            .get("manifest")
            .is_none()
    );
    assert_eq!(
        client
            .get(&format!("/v1/snapshots/{id}?manifest=true"))
            .await?["manifest"],
        manifest
    );
    let job = client
        .post(
            "/v1/jobs",
            json!({"spec":{"kind":"command","command":["true"]}}),
        )
        .await?;
    let job_id = job["id"].as_str().unwrap().to_owned();
    let selected = job_id.clone();
    let artifact_digest = digest.clone();
    let artifact_size = bytes.len();
    let database = gridstator::db::Db::open(&_temp.path().join("controller/controller.db"))?;
    database
        .write(move |connection| {
            let mut job = gridstator::db::get(connection, "jobs", &selected)?.unwrap();
            job["artifacts"] =
                json!([{"name":"a+b/report 🦀.txt","digest":artifact_digest,"size":artifact_size}]);
            gridstator::db::put(connection, "jobs", &selected, &job)
        })
        .await?;
    for name in [
        "a+b/report%20%F0%9F%A6%80.txt",
        "a%2Bb/report%20%F0%9F%A6%80.txt",
    ] {
        let response = client
            .http
            .get(format!(
                "{}/v1/jobs/{job_id}/artifacts/{name}",
                client.config.url
            ))
            .bearer_auth(&client.config.token)
            .send()
            .await?
            .error_for_status()?;
        assert_eq!(response.bytes().await?.as_ref(), bytes);
    }

    server.abort();
    Ok(())
}

async fn upload_blob(client: &Client, digest: &str, bytes: &[u8]) -> Result<reqwest::Response> {
    let mut archive = tar::Builder::new(vec![]);
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    archive.append_data(
        &mut header,
        format!("sha256/{}", digest.trim_start_matches("sha256:")),
        bytes,
    )?;
    Ok(client
        .http
        .post(format!("{}/v1/blobs/upload", client.config.url))
        .bearer_auth(&client.config.token)
        .header("content-type", "application/x-tar")
        .body(archive.into_inner()?)
        .send()
        .await?)
}

#[tokio::test]
async fn source_snapshot_http_rejects_missing_incomplete_corrupt_and_unsafe_manifests() -> Result<()>
{
    let (_temp, server, client) = boot().await?;
    let bytes = b"hello snapshot";
    let digest = gridstator::workspace::digest_of(bytes);
    let manifest = json!({"version":1,"entries":[{"path":"source.txt","type":"file","size":bytes.len(),"digest":digest,"executable":false}]});
    let encoded = serde_json::to_vec(&manifest)?;
    let id = gridstator::workspace::digest_of(&encoded);
    assert_eq!(
        client
            .post("/v1/blobs/missing", json!({"digests":[digest,id]}))
            .await?["missing"],
        json!([digest, id])
    );
    let absent = client
        .post(
            "/v1/snapshots",
            json!({"manifestDigest":gridstator::workspace::digest_of(b"none")}),
        )
        .await
        .unwrap_err();
    assert!(
        absent.to_string().starts_with("404"),
        "missing manifest: {absent}"
    );
    assert!(
        upload_blob(&client, &id, &encoded)
            .await?
            .status()
            .is_success()
    );
    let incomplete = client
        .post("/v1/snapshots", json!({"manifestDigest":id}))
        .await
        .unwrap_err();
    assert!(
        incomplete.to_string().starts_with("400"),
        "incomplete snapshot: {incomplete}"
    );
    assert_eq!(
        upload_blob(&client, &digest, b"bad").await?.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let stored: Value = upload_blob(&client, &digest, bytes)
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(stored["stored"], 1);
    let snapshot = client
        .post("/v1/snapshots", json!({"manifestDigest":id}))
        .await?;
    assert_eq!(snapshot["id"], id);
    assert_eq!(snapshot["fileCount"], 1);
    assert_eq!(snapshot["totalBytes"], bytes.len());
    assert_eq!(
        client
            .post(
                "/v1/jobs",
                json!({"spec":{"kind":"command","command":["true"],"snapshot":id}})
            )
            .await?["snapshotId"],
        id
    );
    let mut unsafe_manifest = manifest;
    unsafe_manifest["entries"][0]["path"] = json!("../../etc/passwd");
    let encoded = serde_json::to_vec(&unsafe_manifest)?;
    let id = gridstator::workspace::digest_of(&encoded);
    assert!(
        upload_blob(&client, &id, &encoded)
            .await?
            .status()
            .is_success()
    );
    let invalid = client
        .post("/v1/snapshots", json!({"manifestDigest":id}))
        .await
        .unwrap_err();
    assert!(
        invalid.to_string().starts_with("400"),
        "unsafe manifest: {invalid}"
    );
    assert!(
        invalid.to_string().contains("\"..\" path segment"),
        "unsafe-path diagnostic: {invalid}"
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn source_http_integer_fields_accept_whole_number_json_literals() -> Result<()> {
    let (_temp, server, client) = boot().await?;
    // JSON.parse/Zod treat 1 and 1.0 as the same number; preserve this at the HTTP boundary.
    let request: Value = serde_json::from_str(
        r#"{"kind":"join","name":"whole-number","maxUses":1.0,"ttlSeconds":60.0}"#,
    )?;
    let issued = client.post("/v1/tokens", request).await?;
    assert_eq!(issued["info"]["usesRemaining"], 1);
    let request: Value =
        serde_json::from_str(r#"{"shards":2.0,"spec":{"kind":"command","command":["true"]}}"#)?;
    assert_eq!(client.post("/v1/groups", request).await?["shardTotal"], 2);
    let request: Value = serde_json::from_str(r#"{"jobRetentionDays":7.0}"#)?;
    assert_eq!(
        client
            .request(reqwest::Method::PATCH, "/v1/settings", Some(request))
            .await?["jobRetentionDays"],
        7
    );
    server.abort();
    Ok(())
}
