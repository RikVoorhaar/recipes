#[path = "support/mcp.rs"]
mod mcp;

use recipe_app::{
    app_with_public_base_url, config::parse_public_base_url, configure_tokens, database,
    storage::FileImageStorage,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;

async fn server(configured: bool) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pool = database::connect(&dir.path().join("db.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    configure_tokens(&pool, &["owner:secret".into()])
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = app_with_public_base_url(
        pool,
        Arc::new(FileImageStorage::new(dir.path().join("images"))),
        None,
        vec![],
        configured.then(|| parse_public_base_url(&base).unwrap()),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (dir, base)
}

fn no_links(value: &Value) {
    assert!(value.get("public_url").is_none(), "{value}");
    assert!(value.get("pdf_url").is_none(), "{value}");
}

#[tokio::test]
async fn links_only_for_current_public_recipes() {
    for configured in [false, true] {
        let (_dir, base) = server(configured).await;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::HOST, "untrusted.example".parse().unwrap());
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        let call = |name, args| mcp::tool(&client, &base, "secret", name, args);
        let draft = call(
            "save_draft",
            json!({"document":{"title":"Soup","servings":2,"body":"Cook @carrot{1}."}}),
        )
        .await;
        no_links(&draft);
        no_links(&draft["document"]);
        let draft_id = draft["id"].as_str().unwrap();
        let read = call("get_draft", json!({"id":draft_id})).await;
        no_links(&read);
        let saved = call(
            "publish_draft",
            json!({"id":draft_id,"expected_revision":1}),
        )
        .await;
        let recipe = &saved["recipe"];
        let id = recipe["id"].as_str().unwrap();
        let revision = recipe["revision"].as_str().unwrap();
        let get = call("get_recipe", json!({"id":id})).await;
        let list = call("list_recipes", json!({})).await;
        let search = call("search_recipes", json!({})).await;
        for item in [recipe, &get, &list[0], &search[0]] {
            if configured {
                assert_eq!(item["public_url"], format!("{base}/recipes/{id}"));
                assert_eq!(item["pdf_url"], format!("{base}/recipes/{id}.pdf"));
            } else {
                no_links(item);
            }
        }
        if configured {
            for field in ["public_url", "pdf_url"] {
                let response = client
                    .get(recipe[field].as_str().unwrap())
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200);
                assert!(response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with(if field == "pdf_url" {
                        "application/pdf"
                    } else {
                        "text/html"
                    }));
            }
        }
        let history = call("get_revision", json!({"id":id,"revision":revision})).await;
        no_links(&history);
        let edit = call(
            "save_draft",
            json!({"recipe_id":id,"document":{"title":"Updated Soup"}}),
        )
        .await;
        no_links(&edit);
        no_links(&edit["document"]);
        let updated = call(
            "publish_draft",
            json!({"id":edit["id"],"expected_revision":1}),
        )
        .await;
        no_links(&call("get_revision", json!({"id":id,"revision":revision})).await);
        let deleted = call(
            "delete_recipe",
            json!({"id":id,"base_revision":updated["recipe"]["revision"]}),
        )
        .await;
        no_links(&deleted);
        no_links(&call("get_recipe", json!({"id":id})).await);
        let search = call("search_recipes", json!({"query":{"include_deleted":true}})).await;
        assert_eq!(search.as_array().unwrap().len(), 1);
        no_links(&search[0]);
        assert!(call("list_recipes", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn base_url_validation_and_prefix() {
    for invalid in [
        "",
        "ftp://example.com",
        "https://",
        "https://user@example.com",
        "https://example.com/?q=1",
        "https://example.com/#x",
        " https://example.com",
        "https://example.com/a b",
    ] {
        assert!(parse_public_base_url(invalid).is_none(), "{invalid}");
    }
    assert_eq!(
        parse_public_base_url("https://example.com/prefix")
            .unwrap()
            .as_str(),
        "https://example.com/prefix/"
    );
}
