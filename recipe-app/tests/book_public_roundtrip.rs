#[path = "support/mcp.rs"]
mod mcp;
#[allow(dead_code)]
mod support;

use recipe_app::{book_archive, book_snapshot, database, storage::FileImageStorage};
use serde_json::{json, Value};
use std::{io::Cursor, sync::Arc};

async fn call(base: &str, name: &str, arguments: Value) -> Value {
    mcp::tool(
        &reqwest::Client::new(),
        base,
        "test-secret",
        name,
        arguments,
    )
    .await
}

async fn publish(base: &str, document: Value) -> Value {
    let draft = call(base, "save_draft", json!({"document": document})).await;
    call(
        base,
        "publish_draft",
        json!({"id":draft["id"],"expected_revision":draft["revision"]}),
    )
    .await["recipe"]
        .clone()
}

#[tokio::test]
async fn public_and_mcp_outputs_survive_readable_book_roundtrip() {
    let source = support::TestApp::start().await;
    let base = source.url("");
    let seed = publish(&base, json!({"title":"Tomato soup","description":"A warming lunch","servings":2,"tags":["soup"],"body":"Cook @tomato{200%g} with @salt{2%g}."})).await;
    let ingredients = call(&base, "list_ingredients", json!({})).await;
    let tomato = ingredients
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "tomato")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let added = mcp::call(
        &source.client(),
        &base,
        "test-secret",
        "add_ingredient_alias",
        json!({"id":tomato,"alias":"pomodoro"}),
    )
    .await;
    assert_ne!(added["isError"], true);
    let draft = call(&base, "save_draft", json!({"recipe_id":seed["id"],"document":{"body":"Cook @pomodoro{200%g} with @salt{2%g}."}})).await;
    let recipe = call(
        &base,
        "publish_draft",
        json!({"id":draft["id"],"expected_revision":draft["revision"]}),
    )
    .await["recipe"]
        .clone();
    let removed = mcp::call(
        &source.client(),
        &base,
        "test-secret",
        "remove_ingredient_alias",
        json!({"id":tomato,"alias":"pomodoro"}),
    )
    .await;
    assert_ne!(removed["isError"], true);
    let mut jpeg = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(16, 16)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    source.install_image_fixture("img_roundtrip", jpeg.get_ref());
    let attached = call(
        &base,
        "attach_image",
        json!({"id":recipe["id"],"base_revision":recipe["revision"],"image_ref":"img_roundtrip"}),
    )
    .await;
    let id = attached["id"].as_str().unwrap();
    let old = publish(
        &base,
        json!({"title":"Retired soup","servings":1,"body":"Heat water."}),
    )
    .await;
    call(
        &base,
        "delete_recipe",
        json!({"id":old["id"],"base_revision":old["revision"]}),
    )
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("book");
    let snapshot =
        book_snapshot::capture(source.pool(), &FileImageStorage::new(source.content_dir()))
            .await
            .unwrap();
    book_archive::write_tree(&tree, snapshot).unwrap();
    let db = tmp.path().join("restored.sqlite");
    let content = tmp.path().join("content");
    book_archive::import_tree(&tree, &db, &content)
        .await
        .unwrap();
    let target = database::connect(&db).await.unwrap();
    recipe_app::configure_tokens(&target, &["integration:test-secret".into()])
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app =
        recipe_app::app_with_storage(target.clone(), Arc::new(FileImageStorage::new(&content)));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let restored = format!("http://{addr}");
    let client = reqwest::Client::new();
    for path in [
        "/".to_owned(),
        "/recipes?tag=soup".to_owned(),
        "/ingredients".to_owned(),
        format!("/recipes/{id}"),
        format!("/recipes/{id}?servings=4"),
        format!("/recipes/{id}.pdf?servings=4"),
        "/images/img_roundtrip".to_owned(),
    ] {
        let a = client.get(format!("{base}{path}")).send().await.unwrap();
        let b = client
            .get(format!("{restored}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(a.status(), 200, "{path}");
        assert_eq!(a.status(), b.status(), "{path}");
        let bytes = a.bytes().await.unwrap();
        assert_eq!(bytes, b.bytes().await.unwrap(), "{path}");
        if path.ends_with("?servings=4") && !path.contains(".pdf") {
            assert!(String::from_utf8_lossy(&bytes).contains("400 g"));
        }
        if path == "/ingredients" {
            let html = String::from_utf8_lossy(&bytes);
            assert!(html.contains("tomato") && html.contains(id));
        }
        if path.contains(".pdf") {
            assert!(bytes.starts_with(b"%PDF-"));
        }
        if path.starts_with("/images/") {
            assert_eq!(bytes.as_ref(), jpeg.get_ref());
        }
    }
    let query = json!({"query":{"ingredients":tomato}});
    let source_search = call(&base, "search_recipes", query.clone()).await;
    assert_eq!(source_search.as_array().unwrap().len(), 1);
    assert_eq!(
        source_search,
        call(&restored, "search_recipes", query).await
    );
    let deleted_id = old["id"].as_str().unwrap();
    assert_eq!(
        client
            .get(format!("{restored}/recipes/{deleted_id}"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let revision: String =
        sqlx::query_scalar("SELECT current_revision_id FROM recipes WHERE id=? AND deleted=1")
            .bind(deleted_id)
            .fetch_one(&target)
            .await
            .unwrap();
    let undeleted = call(
        &restored,
        "restore_recipe",
        json!({"id":deleted_id,"base_revision":revision}),
    )
    .await;
    assert_eq!(undeleted["deleted"], false);
    assert_eq!(
        client
            .get(format!("{restored}/recipes/{deleted_id}"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    server.abort();
    target.close().await;
}
