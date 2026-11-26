use axum::{routing::post, Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{ImageBuffer, ImageFormat, Rgb};
use recipe_app::{
    app_with_mcp_origins, configure_tokens, cover::Generator, database, storage::FileImageStorage,
};
use serde_json::{json, Value};
use std::{io::Cursor, sync::Arc};
use tokio::net::TcpListener;

async fn server() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut png = Cursor::new(Vec::new());
    ImageBuffer::from_pixel(2, 2, Rgb::<u8>([180, 80, 20]))
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let encoded = STANDARD.encode(png.into_inner());
    let provider = Router::new().route(
        "/",
        post(move || {
            let encoded = encoded.clone();
            async move { Json(json!({"data":[{"b64_json":encoded}]})) }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let generator =
        Generator::with_test_endpoint("test-key".into(), None, None, &provider_url).unwrap();
    let pool = database::connect(&dir.path().join("db.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    configure_tokens(&pool, &["owner:first".into(), "other:second".into()])
        .await
        .unwrap();
    let app = app_with_mcp_origins(
        pool,
        Arc::new(FileImageStorage::new(dir.path().join("images"))),
        Some(Arc::new(generator)),
        vec!["http://localhost:6274".into()],
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (dir, url)
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    header: &str,
    value: &str,
    method: &str,
    params: Value,
) -> Value {
    let response = client
        .post(url)
        .header(header, value)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    response.json().await.unwrap()
}

async fn tool(
    client: &reqwest::Client,
    url: &str,
    header: &str,
    value: &str,
    name: &str,
    args: Value,
) -> Value {
    rpc(
        client,
        url,
        header,
        value,
        "tools/call",
        json!({"name":name,"arguments":args}),
    )
    .await["result"]
        .clone()
}

fn tool_json(result: &Value) -> Value {
    assert_ne!(result["isError"], true, "{result}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn complete_workflow_for_both_headers_and_private_drafts() {
    let (_dir, url) = server().await;
    let client = reqwest::Client::new();
    for (header, value) in [("authorization", "Bearer first"), ("x-api-key", "second")] {
        let init = rpc(&client, &url, header, value, "initialize", json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}})).await;
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        let notification = client
            .post(&url)
            .header(header, value)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap();
        assert_eq!(notification.status(), 202);
        let list = rpc(&client, &url, header, value, "tools/list", json!({})).await;
        assert!(!list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "upload_image"));
        assert_ne!(
            tool(&client, &url, header, value, "get_guidelines", json!({})).await["isError"],
            true
        );
        let document = json!({"title":"Tomato soup","servings":2,"body":"Cook @tomato{2}."});
        let draft = tool_json(
            &tool(
                &client,
                &url,
                header,
                value,
                "save_draft",
                json!({"document":document}),
            )
            .await,
        );
        let (other_header, other_value) = if value == "second" {
            ("authorization", "Bearer first")
        } else {
            ("x-api-key", "second")
        };
        let other = tool(
            &client,
            &url,
            other_header,
            other_value,
            "get_draft",
            json!({"id":draft["id"]}),
        )
        .await;
        assert_eq!(other["isError"], true);
        assert_ne!(
            tool(&client, &url, header, value, "validate_recipe", document).await["isError"],
            true
        );
        let cover = tool(&client, &url, header, value, "generate_cover", json!({"draft_id":draft["id"],"expected_revision":draft["revision"],"dish_description":"Tomato soup"})).await;
        let cover_data = tool_json(&cover);
        let published = tool_json(&tool(&client, &url, header, value, "publish_draft", json!({"id":draft["id"],"expected_revision":draft["revision"],"image_ref":cover_data["image_ref"]})).await);
        assert!(published["recipe"]["image"].is_object(), "{published}");
    }
}

#[tokio::test]
async fn transport_security_and_protocol_errors() {
    let (_dir, url) = server().await;
    let client = reqwest::Client::new();
    let body = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
    assert_eq!(client.get(&url).send().await.unwrap().status(), 405);
    for req in [
        client.post(&url),
        client.post(&url).bearer_auth("wrong"),
        client.post(&url).header("authorization", "Basic first"),
        client
            .post(&url)
            .bearer_auth("first")
            .header("x-api-key", "first"),
        client
            .post(&url)
            .header("authorization", "Bearer first")
            .header("authorization", "Bearer first"),
        client.post(&url).header("x-api-key", "first,second"),
    ] {
        let response = req.json(&body).send().await.unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
    }
    for origin in [
        "null",
        "http://localhost:6274.evil",
        "http://localhost:6274/path",
        "https://evil.example",
    ] {
        assert_eq!(
            client
                .post(&url)
                .bearer_auth("first")
                .header("origin", origin)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let spoofed = client
        .post(&url)
        .bearer_auth("first")
        .header("host", "evil.example")
        .header("origin", "http://evil.example")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(spoofed.status(), 403);
    assert_eq!(
        client
            .get(&url)
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(&url)
        .bearer_auth("first")
        .header("origin", "http://localhost:6274")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://localhost:6274"
    );
    let response = client
        .request(reqwest::Method::OPTIONS, &url)
        .header("origin", "http://localhost:6274")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "authorization,content-type",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://localhost:6274"
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("first")
            .header("mcp-protocol-version", "unsupported")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("first")
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
}
