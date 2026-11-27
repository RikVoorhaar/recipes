#[path = "support/mcp.rs"]
mod mcp;
#[allow(dead_code)]
mod support;

use serde_json::json;
use std::{
    ffi::{c_int, c_void},
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

static BUSY_OBSERVED: AtomicBool = AtomicBool::new(false);

// Test-only replacement for SQLite's timeout handler. It signals that the app
// actually encountered the other process's lock, then retries for up to 5s.
unsafe extern "C" fn observe_busy(_: *mut c_void, attempts: c_int) -> c_int {
    BUSY_OBSERVED.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(10));
    i32::from(attempts < 500)
}

async fn observe_pool_contention(pool: &sqlx::SqlitePool) {
    let mut connections = Vec::new();
    for _ in 0..pool.options().get_max_connections() {
        connections.push(pool.acquire().await.unwrap());
    }
    for connection in &mut connections {
        let mut handle = connection.lock_handle().await.unwrap();
        // SAFETY: the locked handle is a valid SQLite connection; the callback
        // uses no context pointer and its static state outlives every connection.
        let result = unsafe {
            libsqlite3_sys::sqlite3_busy_handler(
                handle.as_raw_handle().as_ptr(),
                Some(observe_busy),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, libsqlite3_sys::SQLITE_OK);
    }
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for process synchronization");
}

#[tokio::test]
#[ignore]
async fn hold_other_process_writer() {
    let db = std::env::var_os("CONTENTION_DB").unwrap();
    let dir = std::path::PathBuf::from(std::env::var_os("CONTENTION_DIR").unwrap());
    let pool = recipe_app::database::connect(Path::new(&db)).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE ingredients SET name = name")
        .execute(&mut *tx)
        .await
        .unwrap();
    std::fs::write(dir.join("locked"), "").unwrap();
    wait_for_file(&dir.join("release")).await;
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn ingredient_alias_waits_for_another_process_writer() {
    let app = support::TestApp::start().await;
    let client = app.client();
    let base = app.url("");
    let draft = mcp::tool(
        &client,
        &base,
        "test-secret",
        "save_draft",
        json!({
            "document":{"title":"Contention soup","servings":2,"body":"@tomato{1%g}"}
        }),
    )
    .await;
    mcp::tool(
        &client,
        &base,
        "test-secret",
        "publish_draft",
        json!({
            "id":draft["id"],"expected_revision":draft["revision"]
        }),
    )
    .await;
    let ingredients = mcp::tool(&client, &base, "test-secret", "list_ingredients", json!({})).await;
    let id = ingredients[0]["id"].as_str().unwrap().to_owned();

    observe_pool_contention(app.pool()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "hold_other_process_writer", "--ignored"])
        .env("CONTENTION_DB", app.database_path())
        .env("CONTENTION_DIR", dir.path())
        .spawn()
        .unwrap();
    wait_for_file(&dir.path().join("locked")).await;
    let visible = tokio::time::timeout(
        Duration::from_secs(2),
        mcp::tool(
            &client,
            &base,
            "test-secret",
            "get_ingredient",
            json!({"id":id}),
        ),
    )
    .await
    .expect("reader blocked by writer");
    assert_eq!(visible["name"], "tomato");
    let request_id = id.clone();
    let mut request = tokio::spawn(async move {
        mcp::call(
            &client,
            &base,
            "test-secret",
            "add_ingredient_alias",
            json!({"id":request_id,"alias":"pomodoro"}),
        )
        .await
    });
    let contention = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if BUSY_OBSERVED.load(Ordering::SeqCst) {
                break Ok(());
            }
            if request.is_finished() {
                break Err("mutation completed without encountering the writer lock");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    std::fs::write(dir.path().join("release"), "").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(contention.expect("mutation never reached the lock"), Ok(()));
    let result = tokio::time::timeout(Duration::from_secs(6), &mut request)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(result["isError"], true, "{result}");
    let detail = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "get_ingredient",
        json!({"id":id}),
    )
    .await;
    assert_eq!(detail["aliases"], json!(["pomodoro"]));
}
