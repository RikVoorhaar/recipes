#[path = "support/mcp.rs"]
mod mcp;
#[allow(dead_code)]
mod support;

use serde_json::json;
use std::{path::Path, process::Command, time::Duration};

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
    let early = tokio::time::timeout(Duration::from_millis(500), &mut request).await;
    std::fs::write(dir.path().join("release"), "").unwrap();
    assert!(child.wait().unwrap().success());
    match early {
        Ok(result) => panic!(
            "ingredient mutation finished while writer held lock: {:?}",
            result.unwrap()
        ),
        Err(_) => {
            let result = tokio::time::timeout(Duration::from_secs(6), request)
                .await
                .unwrap()
                .unwrap();
            assert_ne!(result["isError"], true, "{result}");
        }
    }
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
