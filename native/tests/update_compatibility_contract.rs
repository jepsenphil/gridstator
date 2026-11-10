use gridstator::client::{Client, Config, ControllerError};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn seven_frozen_update_requests_preserve_indefinite_draining_without_retry() {
    let fixture: Value =
        serde_json::from_str(include_str!("../contracts/update-compatibility-v1.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = case["response"].clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let (head, body_offset, length) = loop {
                let mut chunk = [0; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0, "truncated update request");
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8(bytes[..index].to_vec()).unwrap();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (head, index + 4, length);
                }
            };
            while bytes.len() < body_offset + length {
                let mut chunk = [0; 4096];
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0, "truncated update body");
                bytes.extend_from_slice(&chunk[..count]);
            }
            let body: Value =
                serde_json::from_slice(&bytes[body_offset..body_offset + length]).unwrap();
            let body_bytes = serde_json::to_vec(&response["body"]).unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response["status"],
                        body_bytes.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&body_bytes).await.unwrap();
            drop(stream);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                    .await
                    .is_err(),
                "update request was retried"
            );
            let mut request = head.lines().next().unwrap().split_whitespace();
            json!({"method":request.next().unwrap(),"path":request.next().unwrap(),"body":body})
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
        let result = client.start_update(case["input"].clone()).await;
        assert_eq!(server.await.unwrap(), case["requests"][0], "{}", case["id"]);
        assert_eq!(case["requests"].as_array().unwrap().len(), 1);
        if let Some(expected) = case.get("value") {
            assert_eq!(result.unwrap(), *expected, "{}", case["id"]);
        } else {
            let error = result.unwrap_err();
            let error = error.downcast_ref::<ControllerError>().unwrap();
            assert_eq!(
                error.code.as_deref(),
                case["error"]["code"].as_str(),
                "{}",
                case["id"]
            );
            assert_eq!(
                error.message,
                case["error"]["message"].as_str().unwrap(),
                "{}",
                case["id"]
            );
            if let Some(expected) = case["error"].get("cause") {
                let cause = error
                    .cause
                    .as_ref()
                    .unwrap()
                    .downcast_ref::<ControllerError>()
                    .unwrap();
                assert_eq!(cause.code.as_deref(), expected["code"].as_str());
                assert_eq!(cause.message, expected["message"].as_str().unwrap());
                assert_eq!(
                    error.hint.as_deref(),
                    Some(
                        "Install a current controller native release bundle with `gridstator controller publish-build <bundle.tar.gz>`."
                    )
                );
            } else {
                assert!(error.cause.is_none());
                assert_eq!(error.hint.as_deref(), case["error"]["hint"].as_str());
            }
        }
    }
}
