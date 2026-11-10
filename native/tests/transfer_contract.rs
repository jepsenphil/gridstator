use anyhow::Result;
use gridstator::{
    client::{Client, Config},
    transfer,
    workspace::BlobStore,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn cancelling_a_stalled_download_releases_the_blocking_reader() -> Result<()> {
    // An explicit runtime exposes leaked blocking work: shutdown must finish promptly.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()?;
    let started = std::time::Instant::now();
    runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut stream,_) = listener.accept().await?;
            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer).await?;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-tar\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n").await?;
            std::future::pending::<()>().await;
            Ok::<(),anyhow::Error>(())
        });
        let temporary=tempfile::tempdir()?;
        let store=BlobStore::new(temporary.path());store.init()?;
        // This test-only HTTP endpoint deliberately never completes its response body.
        let client=Client{http:reqwest::Client::new(),config:Config{version:1,url:format!("http://{address}"),ca_pem:String::new(),ca_fingerprint:String::new(),token:"test".into(),token_name:None}};
        let digests=vec![format!("sha256:{}","0".repeat(64))];
        assert!(tokio::time::timeout(Duration::from_millis(200),transfer::download_blobs(&client,&store,&digests)).await.is_err());
        // With one blocking thread, this only starts once the tar reader has unwound.
        // Keep the stalled socket open until after this observation.
        let released = tokio::time::timeout(Duration::from_millis(300), tokio::task::spawn_blocking(||true)).await;
        server.abort();
        anyhow::ensure!(released.is_ok(), "cancelled transfer retained its blocking reader");
        Ok::<(),anyhow::Error>(())
    })?;
    runtime.shutdown_timeout(Duration::from_secs(1));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancelled transfer retained blocking work"
    );
    Ok(())
}
