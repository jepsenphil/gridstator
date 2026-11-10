use anyhow::Result;
use gridstator::db::{self, Db};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn independent_database_owners_serialize_read_modify_write_transactions() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("controller.db");
    let first = Db::open(&path)?;
    let second = Db::open(&path)?;
    first
        .write(|connection| db::put(connection, "test", "counter", &json!(0)))
        .await?;
    let (entered, started) = tokio::sync::oneshot::channel();
    let mutation = tokio::spawn(async move {
        first
            .write(move |connection| {
                let value = db::get(connection, "test", "counter")?
                    .unwrap()
                    .as_u64()
                    .unwrap();
                let _ = entered.send(());
                std::thread::sleep(Duration::from_millis(100));
                db::put(connection, "test", "counter", &json!(value + 1))
            })
            .await
    });
    started.await?;
    second
        .write(|connection| {
            let value = db::get(connection, "test", "counter")?
                .unwrap()
                .as_u64()
                .unwrap();
            db::put(connection, "test", "counter", &json!(value + 1))
        })
        .await?;
    mutation.await??;
    assert_eq!(
        second
            .call(|connection| db::get(connection, "test", "counter"))
            .await?,
        Some(json!(2))
    );
    Ok(())
}

#[tokio::test]
async fn instance_lease_survives_cancelled_caller_until_queued_mutation_finishes() -> Result<()> {
    use std::sync::{Arc, Condvar, Mutex};
    let temporary = tempfile::tempdir()?;
    let lock_path = temporary.path().join("instance.lock");
    let lease = Arc::new(std::fs::File::create(&lock_path)?);
    fs2::FileExt::lock_exclusive(lease.as_ref())?;
    let database = Db::open_with_guard(&temporary.path().join("controller.db"), lease.clone())?;
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mutation_gate = gate.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let caller = tokio::spawn(async move {
        database
            .write(move |connection| {
                let _ = entered.send(());
                let (lock, wake) = mutation_gate.as_ref();
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
                db::put(connection, "test", "finished", &json!(true))
            })
            .await
    });
    started.await?;
    drop(lease);
    caller.abort();
    let _ = caller.await;
    let challenger = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)?;
    let retained = fs2::FileExt::try_lock_exclusive(&challenger).is_err();
    let (lock, wake) = gate.as_ref();
    *lock.lock().unwrap() = true;
    wake.notify_all();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fs2::FileExt::try_lock_exclusive(&challenger).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert!(
        retained,
        "instance lock released while cancelled database mutation was still running"
    );
    Ok(())
}
