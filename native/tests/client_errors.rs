use gridstator::client::{Client, Config, ControllerError};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
async fn response(status: u16, body: Vec<u8>) -> anyhow::Result<anyhow::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = [0; 4096];
        let count = stream.read(&mut request).await?;
        anyhow::ensure!(count > 0, "missing HTTP request");
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        let _ = stream.write_all(&body).await;
        Ok::<_, anyhow::Error>(())
    });
    let client = Client {
        http: reqwest::Client::new(),
        config: Config {
            version: 1,
            url: format!("http://{address}"),
            ca_pem: String::new(),
            ca_fingerprint: String::new(),
            token: "test-only".into(),
            token_name: None,
        },
    };
    let error = client.get("/source-error").await.unwrap_err();
    server.await??;
    Ok(error)
}
#[tokio::test]
async fn source_error_bodies_retain_codes_optional_hints_and_details() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../contracts/accepted-v1/errors-and-units.json"
    ))
    .unwrap();
    for case in fixture["bodies"].as_array().unwrap() {
        let error = response(
            case["status"].as_u64().unwrap() as u16,
            serde_json::to_vec(&case["body"]).unwrap(),
        )
        .await
        .unwrap();
        let remote = error.downcast_ref::<ControllerError>().unwrap();
        assert_eq!(
            remote.code.as_deref(),
            case["body"]["error"]["code"].as_str()
        );
        assert_eq!(
            remote.message,
            case["body"]["error"]["message"].as_str().unwrap()
        );
        assert_eq!(
            remote.hint.as_deref(),
            case["body"]["error"]["hint"].as_str()
        );
        assert_eq!(
            remote.details,
            case["body"]["error"].get("details").cloned()
        );
    }
    for body in [
        json!({"error":{"code":"UNKNOWN","hint":42}}),
        json!({"error":{"message":"test","hint":"help","details":null}}),
        json!({}),
    ] {
        let error = response(400, serde_json::to_vec(&body).unwrap())
            .await
            .unwrap();
        let remote = error.downcast_ref::<ControllerError>().unwrap();
        assert_eq!(remote.code.as_deref(), Some("BAD_REQUEST"));
        assert_eq!(remote.hint.as_deref(), body["error"]["hint"].as_str());
        assert_eq!(remote.details, body["error"].get("details").cloned());
    }
}
#[tokio::test]
async fn response_size_limits_match_the_source_client() {
    let error = response(503, vec![b'x'; 1024 * 1024 + 1]).await.unwrap();
    let remote = error.downcast_ref::<ControllerError>().unwrap();
    assert_eq!(remote.code.as_deref(), Some("UNAVAILABLE"));
    assert_eq!(remote.message, "Controller responded with HTTP 503");
    assert!(remote.hint.is_none());
    assert!(remote.details.is_none());
    let error = response(200, vec![b'x'; 16 * 1024 * 1024 + 1])
        .await
        .unwrap();
    let remote = error.downcast_ref::<ControllerError>().unwrap();
    assert_eq!(remote.code.as_deref(), Some("PAYLOAD_TOO_LARGE"));
    assert_eq!(remote.message, "Response too large");
}

#[tokio::test]
async fn untrusted_controller_certificate_is_an_auth_failure_before_bearer_delivery() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = tempfile::tempdir().unwrap();
    let trusted = temp.path().join("trusted");
    let attacker = temp.path().join("attacker");
    gridstator::pki::generate(&trusted, "trusted", &[]).unwrap();
    gridstator::pki::generate(&attacker, "attacker", &[]).unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        attacker.join("pki/server.pem"),
        attacker.join("pki/server.key"),
    )
    .await
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let delivered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = delivered.clone();
    let app = axum::Router::new().fallback(move || {
        let count = count.clone();
        async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            axum::Json(json!({}))
        }
    });
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)
            .serve(app.into_make_service())
            .await
    });
    let ca = std::fs::read_to_string(trusted.join("pki/ca.pem")).unwrap();
    let client = Client::new(Config {
        version: 1,
        url: format!("https://{address}"),
        ca_fingerprint: gridstator::pki::fingerprint(&ca).unwrap(),
        ca_pem: ca,
        token: "bearer-must-not-reach-untrusted-server".into(),
        token_name: None,
    })
    .unwrap();
    let error = client.get("/v1/jobs").await.unwrap_err();
    let config = temp.path().join("client.json");
    gridstator::state::write_json(&config, &serde_json::to_value(&client.config).unwrap()).unwrap();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_gridstator"))
            .args(["status", "--json"])
            .env("GRIDSTATOR_CONFIG", config)
            .env_remove("GRIDSTATOR_URL")
            .env_remove("GRIDSTATOR_TOKEN")
            .env_remove("GRIDSTATOR_CA_FILE")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
    assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 0);
    let remote = error.downcast_ref::<ControllerError>().unwrap_or_else(|| {
        panic!("certificate error lost the source auth classification: {error:?}")
    });
    assert_eq!(remote.code.as_deref(), Some("TLS_PIN_MISMATCH"));
    assert!(remote.hint.as_deref().unwrap().contains("log in again"));
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not signed by the pinned cluster CA"));
    assert!(stderr.contains("hint: If the controller was re-initialized"));
    assert!(!stderr.contains("bearer-must-not-reach-untrusted-server"));
}
