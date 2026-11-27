use recipe_app::database;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

async fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// Invoked in a separate OS process by the test below.
#[tokio::test]
#[ignore]
async fn competing_writer() {
    let dir = std::path::PathBuf::from(std::env::var_os("SQLITE_TEST_DIR").unwrap());
    let pool = database::connect(&dir.join("app.sqlite")).await.unwrap();
    std::fs::write(dir.join("ready"), "").unwrap();
    wait_for_file(&dir.join("go")).await;
    std::fs::write(dir.join("attempting"), "").unwrap();
    sqlx::query("INSERT INTO contention VALUES (2)")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn in_memory_database_still_works() {
    let pool = database::connect(Path::new(":memory:")).await.unwrap();
    let result: i64 = sqlx::query_scalar("SELECT 42")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result, 42);
}

#[tokio::test]
async fn independent_process_waits_for_writer_and_uses_shared_pragmas() {
    let dir = tempfile::tempdir().unwrap();
    let pool = database::connect(&dir.path().join("app.sqlite"))
        .await
        .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    let timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(timeout, 5000);
    assert_eq!(foreign_keys, 1);
    let mut second_connection = pool.acquire().await.unwrap();
    let timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(&mut *second_connection)
        .await
        .unwrap();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *second_connection)
        .await
        .unwrap();
    assert_eq!(timeout, 5000);
    assert_eq!(foreign_keys, 1);
    drop(second_connection);
    sqlx::query("CREATE TABLE contention (id INTEGER PRIMARY KEY)")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "competing_writer", "--ignored"])
        .env("SQLITE_TEST_DIR", dir.path())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_file(&dir.path().join("ready")).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO contention VALUES (1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    std::fs::write(dir.path().join("go"), "").unwrap();
    wait_for_file(&dir.path().join("attempting")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(child.try_wait().unwrap().is_none(), "writer did not wait");
    tx.commit().await.unwrap();
    assert!(child.wait().unwrap().success(), "competing writer failed");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contention")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}
