use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use recipe_app::{
    app_with_storage, app_with_storage_and_cover_generator, configure_tokens, cover::Generator,
    database, storage::FileImageStorage,
};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
};

async fn rpc(
    input: &mut ChildStdin,
    output: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    id: u64,
    method: &str,
    params: Value,
) -> Value {
    let message = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
    input
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
    input.flush().await.unwrap();
    serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap()
}

async fn fake_draft_cover(
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(
        headers.get(header::AUTHORIZATION).unwrap(),
        "Bearer fake-token"
    );
    assert_eq!(body["expected_revision"], 4);
    assert_eq!(body["dish_description"], "Roasted squash");
    if id == "evil-id" {
        return Json(
            json!({"image_ref":"opaque-ref","preview_url":"https://example.invalid/image"}),
        );
    }
    assert_eq!(id, "draft-id");
    Json(
        json!({"image_ref":"opaque-ref","preview_url":"/api/v1/drafts/draft-id/covers/opaque-ref"}),
    )
}

async fn fake_recipe_cover(
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(
        headers.get(header::AUTHORIZATION).unwrap(),
        "Bearer fake-token"
    );
    assert_eq!(id, "recipe");
    assert_eq!(body["base_revision"], "rev-2");
    assert_eq!(body["replace"], true);
    Json(json!({"id":"recipe","revision":"rev-3","image":{"ref":"opaque-ref"}}))
}

async fn fake_preview(
    headers: HeaderMap,
) -> (StatusCode, [(header::HeaderName, &'static str); 1], Vec<u8>) {
    assert_eq!(
        headers.get(header::AUTHORIZATION).unwrap(),
        "Bearer fake-token"
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/jpeg")],
        vec![0xff, 0xd8, 0xff, 0xd9],
    )
}

async fn counted_cover(
    State((requests, _location)): State<(Arc<AtomicUsize>, String)>,
    Path(_id): Path<String>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    requests.fetch_add(1, Ordering::SeqCst);
    Json(json!({"image_ref":"opaque","preview_url":"/api/v1/drafts/draft/covers/opaque"}))
}

async fn counted_preview(
    State((requests, location)): State<(Arc<AtomicUsize>, String)>,
) -> Redirect {
    requests.fetch_add(1, Ordering::SeqCst);
    Redirect::temporary(&location)
}

async fn redirect_target(
    State((requests, _location)): State<(Arc<AtomicUsize>, String)>,
) -> StatusCode {
    requests.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK
}

async fn external_redirect_target(State(requests): State<Arc<AtomicUsize>>) -> StatusCode {
    requests.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK
}

#[tokio::test]
async fn generate_cover_uses_fake_backend_and_authenticated_relative_preview() {
    let app = Router::new()
        .route("/api/v1/drafts/:id/cover", post(fake_draft_cover))
        .route("/api/v1/drafts/:id/covers/:image_ref", get(fake_preview))
        .route("/api/v1/recipes/:id/cover", post(fake_recipe_cover));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_recipe-mcp"))
        .env("RECIPE_API_BASE_URL", format!("http://{address}"))
        .env("RECIPE_API_TOKEN", "fake-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let catalog = rpc(&mut input, &mut output, 1, "tools/list", json!({})).await;
    let tool = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "generate_cover")
        .unwrap();
    assert_eq!(tool["inputSchema"]["oneOf"].as_array().unwrap().len(), 2);
    let draft = rpc(&mut input, &mut output, 2, "tools/call", json!({"name":"generate_cover","arguments":{"draft_id":"draft-id","expected_revision":4,"dish_description":"Roasted squash"}})).await;
    assert_ne!(draft["result"]["isError"], true, "{draft}");
    assert_eq!(draft["result"]["content"][0]["type"], "text");
    assert_eq!(draft["result"]["content"][1]["type"], "image");
    assert_eq!(draft["result"]["content"][1]["mimeType"], "image/jpeg");
    let recipe = rpc(&mut input, &mut output, 3, "tools/call", json!({"name":"generate_cover","arguments":{"recipe_id":"recipe","base_revision":"rev-2","replace":true,"dish_description":"Roasted squash"}})).await;
    assert_ne!(recipe["result"]["isError"], true);
    assert_eq!(recipe["result"]["content"].as_array().unwrap().len(), 1);
    let malicious = rpc(&mut input, &mut output, 5, "tools/call", json!({"name":"generate_cover","arguments":{"draft_id":"evil-id","expected_revision":4,"dish_description":"Roasted squash"}})).await;
    assert_eq!(malicious["result"]["isError"], true);
    assert!(malicious["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("expected owner-private route"));
    let invalid = rpc(&mut input, &mut output, 4, "tools/call", json!({"name":"generate_cover","arguments":{"draft_id":"draft","expected_revision":4,"recipe_id":"recipe","base_revision":"rev","dish_description":"dish"}})).await;
    assert_eq!(invalid["result"]["isError"], true);
    child.kill().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn malformed_cover_targets_are_rejected_before_any_backend_request() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/api/v1/drafts/:id/cover", post(counted_cover))
        .with_state((requests.clone(), String::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_recipe-mcp"))
        .env("RECIPE_API_BASE_URL", format!("http://{address}"))
        .env("RECIPE_API_TOKEN", "fake-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let bad_arguments = [
        json!({"draft_id":null,"expected_revision":1,"dish_description":"dish"}),
        json!({"draft_id":"draft","expected_revision":null,"dish_description":"dish"}),
        json!({"draft_id":"draft","expected_revision":"1","dish_description":"dish"}),
        json!({"recipe_id":"recipe","base_revision":null,"dish_description":"dish"}),
        json!({"recipe_id":"recipe","base_revision":false,"dish_description":"dish"}),
        json!({"recipe_id":"recipe","base_revision":"rev","replace":null,"dish_description":"dish"}),
        json!({"recipe_id":"recipe","base_revision":"rev","replace":"yes","dish_description":"dish"}),
        json!({"draft_id":"draft","expected_revision":1,"base_revision":null,"dish_description":"dish"}),
        json!({"draft_id":"draft","expected_revision":1,"recipe_id":null,"dish_description":"dish"}),
        json!({"recipe_id":4,"base_revision":"rev","dish_description":"dish"}),
        json!({"draft_id":"draft","expected_revision":1,"dish_description":12}),
        json!({"draft_id":"draft","expected_revision":1,"dish_description":"é".repeat(501)}),
        json!({"recipe_id":"recipe","base_revision":"rev","expected_revision":null,"dish_description":"dish"}),
        json!({"draft_id":"draft","expected_revision":1,"dish_description":"dish","model":"not-supported"}),
        json!({"draft_id":"draft","expected_revision":1,"recipe_id":"recipe","base_revision":"rev","dish_description":"dish"}),
    ];
    for (index, arguments) in bad_arguments.into_iter().enumerate() {
        let result = rpc(
            &mut input,
            &mut output,
            index as u64 + 1,
            "tools/call",
            json!({"name":"generate_cover","arguments":arguments}),
        )
        .await;
        assert_eq!(result["result"]["isError"], true, "{arguments}");
    }
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    child.kill().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn preview_redirects_are_not_followed_for_same_or_external_origin() {
    for external in [false, true] {
        let target_requests = Arc::new(AtomicUsize::new(0));
        let (location, target_server) = if external {
            let target_app = Router::new()
                .route("/redirect-target", get(external_redirect_target))
                .with_state(target_requests.clone());
            let target_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target_address = target_listener.local_addr().unwrap();
            let target_server =
                tokio::spawn(
                    async move { axum::serve(target_listener, target_app).await.unwrap() },
                );
            (
                format!("http://{target_address}/redirect-target"),
                Some(target_server),
            )
        } else {
            ("/redirect-target".to_string(), None)
        };
        let backend_requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/v1/drafts/:id/cover", post(counted_cover))
            .route("/api/v1/drafts/:id/covers/:image_ref", get(counted_preview))
            .route("/redirect-target", get(redirect_target))
            .with_state((backend_requests.clone(), location));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut child = Command::new(env!("CARGO_BIN_EXE_recipe-mcp"))
            .env("RECIPE_API_BASE_URL", format!("http://{address}"))
            .env("RECIPE_API_TOKEN", "fake-token")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        let result = rpc(
            &mut input,
            &mut output,
            1,
            "tools/call",
            json!({"name":"generate_cover","arguments":{"draft_id":"draft","expected_revision":1,"dish_description":"dish"}}),
        )
        .await;
        assert_eq!(result["result"]["isError"], true);
        assert_eq!(backend_requests.load(Ordering::SeqCst), 2);
        assert_eq!(target_requests.load(Ordering::SeqCst), 0);
        child.kill().await.unwrap();
        server.abort();
        if let Some(target_server) = target_server {
            target_server.abort();
        }
    }
}

#[tokio::test]
async fn stdio_cover_generation_publishes_private_candidate_and_survives_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = calls.clone();
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let encoded_image = STANDARD.encode(png.into_inner());
    let provider = Router::new().route(
        "/images",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let calls = provider_calls.clone();
            let image = encoded_image.clone();
            async move {
                assert_eq!(
                    headers.get(header::AUTHORIZATION).unwrap(),
                    "Bearer synthetic-test-key"
                );
                assert_eq!(body["model"], "stdio-fake-model");
                assert!(body["prompt"]
                    .as_str()
                    .unwrap()
                    .contains("TEST HOUSE STYLE"));
                let call = calls.fetch_add(1, Ordering::SeqCst);
                assert!(body["prompt"]
                    .as_str()
                    .unwrap()
                    .contains("Dish: Tomato soup."));
                if call == 1 {
                    return (StatusCode::BAD_GATEWAY, "synthetic provider failure").into_response();
                }
                Json(json!({"data":[{"b64_json":image}]})).into_response()
            }
        }),
    );
    let provider_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_address = provider_listener.local_addr().unwrap();
    let provider_server =
        tokio::spawn(async move { axum::serve(provider_listener, provider).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let pool = database::connect(&temp.path().join("cover-stdio.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    configure_tokens(&pool, &["owner:cover-stdio-secret".into()])
        .await
        .unwrap();
    let generator = Generator::with_test_endpoint(
        "synthetic-test-key".into(),
        Some("stdio-fake-model".into()),
        Some("TEST HOUSE STYLE".into()),
        &format!("http://{provider_address}/images"),
    )
    .unwrap();
    let app = app_with_storage_and_cover_generator(
        pool,
        Arc::new(FileImageStorage::new(temp.path().join("cover-images"))),
        Some(Arc::new(generator)),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_recipe-mcp"))
        .env("RECIPE_API_BASE_URL", format!("http://{address}"))
        .env("RECIPE_API_TOKEN", "cover-stdio-secret")
        // Even if a real provider key is present in the environment, the MCP
        // process only talks to the local recipe API; provider access is injected.
        .env("OPENROUTER_API_KEY", "must-not-be-used")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();

    let created = rpc(
        &mut input,
        &mut output,
        1,
        "tools/call",
        json!({"name":"save_draft","arguments":{"document":{
            "title":"Private tomato soup","servings":2,"body":"Add @tomato{200%g}."
        }}}),
    )
    .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let draft: Value =
        serde_json::from_str(created["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let draft_id = draft["id"].as_str().unwrap();
    let generated = rpc(
        &mut input,
        &mut output,
        2,
        "tools/call",
        json!({"name":"generate_cover","arguments":{
            "draft_id":draft_id,"expected_revision":draft["revision"],
            "dish_description":"Tomato soup."
        }}),
    )
    .await;
    assert_ne!(generated["result"]["isError"], true, "{generated}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let generation_content = generated["result"]["content"].as_array().unwrap();
    let candidate: Value =
        serde_json::from_str(generation_content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(generation_content[1]["type"], "image");
    assert_eq!(generation_content[1]["mimeType"], "image/jpeg");
    let preview_bytes = STANDARD
        .decode(generation_content[1]["data"].as_str().unwrap())
        .unwrap();
    assert!(image::load_from_memory(&preview_bytes).is_ok());
    let image_ref = candidate["image_ref"].as_str().unwrap();
    let public_index = reqwest::get(format!("http://{address}/recipes"))
        .await
        .unwrap();
    assert!(!public_index
        .text()
        .await
        .unwrap()
        .contains("Private tomato soup"));
    assert_eq!(
        reqwest::get(format!("http://{address}/images/{image_ref}"))
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let published = rpc(
        &mut input,
        &mut output,
        3,
        "tools/call",
        json!({"name":"publish_draft","arguments":{
            "id":draft_id,"expected_revision":draft["revision"],"image_ref":image_ref
        }}),
    )
    .await;
    assert_ne!(published["result"]["isError"], true, "{published}");
    let published: Value =
        serde_json::from_str(published["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let recipe = &published["recipe"];
    assert_eq!(recipe["image"]["ref"], image_ref);
    let public_image = reqwest::get(format!("http://{address}/images/{image_ref}"))
        .await
        .unwrap();
    assert_eq!(public_image.status(), reqwest::StatusCode::OK);
    assert!(image::load_from_memory(&public_image.bytes().await.unwrap()).is_ok());
    let consumed = rpc(
        &mut input,
        &mut output,
        4,
        "tools/call",
        json!({"name":"get_draft","arguments":{"id":draft_id}}),
    )
    .await;
    assert_eq!(consumed["result"]["isError"], true);

    let failed_draft = rpc(
        &mut input,
        &mut output,
        5,
        "tools/call",
        json!({"name":"save_draft","arguments":{"document":{
            "title":"Cover failure soup","servings":1,"body":"Heat @water{100%ml}."
        }}}),
    )
    .await;
    let failed_draft: Value = serde_json::from_str(
        failed_draft["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let failed_id = failed_draft["id"].as_str().unwrap();
    let failure = rpc(
        &mut input,
        &mut output,
        6,
        "tools/call",
        json!({"name":"generate_cover","arguments":{
            "draft_id":failed_id,"expected_revision":failed_draft["revision"],
            "dish_description":"Tomato soup."
        }}),
    )
    .await;
    assert_eq!(failure["result"]["isError"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let intact = rpc(
        &mut input,
        &mut output,
        7,
        "tools/call",
        json!({"name":"get_draft","arguments":{"id":failed_id}}),
    )
    .await;
    assert_ne!(intact["result"]["isError"], true);
    let intact: Value =
        serde_json::from_str(intact["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(intact["id"], failed_id);
    let published_without_cover = rpc(
        &mut input,
        &mut output,
        8,
        "tools/call",
        json!({"name":"publish_draft","arguments":{
            "id":failed_id,"expected_revision":failed_draft["revision"]
        }}),
    )
    .await;
    assert_ne!(
        published_without_cover["result"]["isError"], true,
        "{published_without_cover}"
    );
    let published_without_cover: Value = serde_json::from_str(
        published_without_cover["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(published_without_cover["recipe"]["image"].is_null());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    child.kill().await.unwrap();
    app_server.abort();
    provider_server.abort();
}

#[tokio::test]
async fn private_draft_resumes_in_a_new_stdio_process_and_publishes_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    let pool = database::connect(&temp.path().join("resume.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    configure_tokens(&pool, &["owner:resume-secret".into()])
        .await
        .unwrap();
    let app = app_with_storage(
        pool,
        Arc::new(FileImageStorage::new(temp.path().join("images"))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_recipe-mcp"))
            .env("RECIPE_API_BASE_URL", format!("http://{address}"))
            .env("RECIPE_API_TOKEN", "resume-secret")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut first = spawn();
    let mut first_in = first.stdin.take().unwrap();
    let mut first_out = BufReader::new(first.stdout.take().unwrap()).lines();
    let draft = rpc(
        &mut first_in,
        &mut first_out,
        1,
        "tools/call",
        json!({"name":"save_draft","arguments":{"document":{"title":"Resumable soup"}}}),
    )
    .await;
    let draft: Value =
        serde_json::from_str(draft["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    first.kill().await.unwrap();

    let mut second = spawn();
    let mut input = second.stdin.take().unwrap();
    let mut output = BufReader::new(second.stdout.take().unwrap()).lines();
    let read = rpc(
        &mut input,
        &mut output,
        2,
        "tools/call",
        json!({"name":"get_draft","arguments":{"id":draft["id"]}}),
    )
    .await;
    let resumed: Value =
        serde_json::from_str(read["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(resumed["document"]["title"], "Resumable soup");
    let saved = rpc(&mut input,&mut output,3,"tools/call",json!({"name":"save_draft","arguments":{"id":draft["id"],"expected_revision":1,"document":{"servings":2,"body":"Simmer @tomato{100%g}."}}})).await;
    let saved: Value =
        serde_json::from_str(saved["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let validation = rpc(
        &mut input,
        &mut output,
        44,
        "tools/call",
        json!({"name":"validate_recipe","arguments":{
            "title":saved["document"]["title"],
            "servings":saved["document"]["servings"],
            "body":saved["document"]["body"]
        }}),
    )
    .await;
    assert_ne!(validation["result"]["isError"], true);
    let published = rpc(&mut input,&mut output,4,"tools/call",json!({"name":"publish_draft","arguments":{"id":draft["id"],"expected_revision":saved["revision"]}})).await;
    assert_ne!(published["result"]["isError"], true);
    let recipe: Value =
        serde_json::from_str(published["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(recipe["recipe"]["title"], "Resumable soup");
    second.kill().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn stdio_adapter_forwards_to_real_temporary_api() {
    let temp = tempfile::tempdir().unwrap();
    let pool = database::connect(&temp.path().join("db.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    configure_tokens(&pool, &["mcp:stage6-secret".to_string()])
        .await
        .unwrap();
    let app = app_with_storage(
        pool,
        Arc::new(FileImageStorage::new(temp.path().join("images"))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut child: Child = Command::new(env!("CARGO_BIN_EXE_recipe-mcp"))
        .env("RECIPE_API_BASE_URL", format!("http://{address}"))
        .env("RECIPE_API_TOKEN", "stage6-secret")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut output = BufReader::new(stdout).lines();

    let initialized = rpc(
        &mut input,
        &mut output,
        1,
        "initialize",
        json!({"protocolVersion":"2024-11-05"}),
    )
    .await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "recipe-mcp");
    assert_eq!(initialized["result"]["protocolVersion"], "2024-11-05");
    assert!(initialized["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("explicitly call publish_draft"));
    let negotiated = rpc(
        &mut input,
        &mut output,
        13,
        "initialize",
        json!({"protocolVersion":"2025-06-18"}),
    )
    .await;
    assert_eq!(negotiated["result"]["protocolVersion"], "2025-06-18");
    let fallback = rpc(
        &mut input,
        &mut output,
        14,
        "initialize",
        json!({"protocolVersion":"1999-01-01"}),
    )
    .await;
    assert_eq!(fallback["result"]["protocolVersion"], "2025-11-25");
    for notification in ["notifications/initialized", "notifications/not-supported"] {
        input
            .write_all(format!("{}\n", json!({"jsonrpc":"2.0","method":notification})).as_bytes())
            .await
            .unwrap();
    }
    input.flush().await.unwrap();
    let ping = rpc(&mut input, &mut output, 15, "ping", json!({})).await;
    assert_eq!(ping["id"], 15, "notifications must not emit responses");
    let catalog = rpc(&mut input, &mut output, 12, "tools/list", json!({})).await;
    let tools = catalog["result"]["tools"].as_array().unwrap();
    for expected in [
        "search_recipes",
        "get_recipe",
        "save_draft",
        "get_draft",
        "list_drafts",
        "discard_draft",
        "publish_draft",
        "delete_recipe",
        "restore_recipe",
        "list_revisions",
        "get_revision",
        "restore_revision",
        "upload_image",
        "attach_image",
        "list_ingredients",
        "add_ingredient_alias",
        "remove_ingredient_alias",
        "rename_ingredient",
        "merge_ingredients",
        "list_tags",
        "validate_recipe",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "missing tool {expected}"
        );
    }
    for name in ["create_recipe", "update_recipe"] {
        assert!(!tools.iter().any(|tool| tool["name"] == name));
        let removed = rpc(
            &mut input,
            &mut output,
            40,
            "tools/call",
            json!({"name":name,"arguments":{}}),
        )
        .await;
        assert_eq!(removed["result"]["isError"], true);
        assert!(removed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }
    let search_tool = tools
        .iter()
        .find(|tool| tool["name"] == "search_recipes")
        .unwrap();
    let filters = &search_tool["inputSchema"]["properties"]["query"]["properties"];
    for (field, ty) in [
        ("q", "string"),
        ("ingredients", "string"),
        ("ingredient_mode", "string"),
        ("exclude_ingredients", "string"),
        ("tags", "string"),
        ("tag_mode", "string"),
        ("exclude_tags", "string"),
        ("min_servings", "integer"),
        ("max_servings", "integer"),
        ("has_image", "boolean"),
        ("include_deleted", "boolean"),
        ("updated_since", "string"),
        ("sort", "string"),
        ("limit", "integer"),
        ("offset", "integer"),
    ] {
        assert_eq!(filters[field]["type"], ty, "wrong schema for {field}");
    }
    let validation_tool = tools
        .iter()
        .find(|tool| tool["name"] == "validate_recipe")
        .unwrap();
    assert_eq!(
        validation_tool["inputSchema"]["properties"]["target_servings"]["type"],
        "integer"
    );
    for field in [
        "title",
        "description",
        "servings",
        "tags",
        "notes",
        "source_url",
        "image_ref",
        "body",
        "target_servings",
    ] {
        assert!(
            validation_tool["inputSchema"]["properties"]
                .get(field)
                .is_some(),
            "missing validation candidate field {field}"
        );
    }
    let resource = rpc(
        &mut input,
        &mut output,
        2,
        "resources/read",
        json!({"uri":"recipe://guidelines"}),
    )
    .await;
    let guidelines = resource["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let tool_guidelines = rpc(
        &mut input,
        &mut output,
        3,
        "tools/call",
        json!({"name":"get_guidelines","arguments":{}}),
    )
    .await;
    assert_eq!(tool_guidelines["result"]["content"][0]["text"], guidelines);

    let create = rpc(
        &mut input,
        &mut output,
        4,
        "tools/call",
        json!({"name":"save_draft","arguments":{
            "document":{"title":"MCP test dish","servings":2,"body":"Add @tomato{100%g}.","tags":["stdio"]}
        }}),
    )
    .await;
    assert_eq!(create["result"]["isError"], Value::Null);
    let saved: Value =
        serde_json::from_str(create["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let draft_id = saved["id"].as_str().unwrap();
    let recipe_draft = rpc(&mut input,&mut output,41,"tools/call",json!({"name":"publish_draft","arguments":{"id":draft_id,"expected_revision":saved["revision"]}})).await;
    let saved: Value = serde_json::from_str(
        recipe_draft["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let recipe = &saved["recipe"];
    let id = recipe["id"].as_str().unwrap();
    let revision = recipe["revision"].as_str().unwrap();

    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(1, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let image_path = temp.path().join("dish.png");
    std::fs::write(&image_path, png.into_inner()).unwrap();
    let uploaded = rpc(
        &mut input,
        &mut output,
        5,
        "tools/call",
        json!({"name":"upload_image","arguments":{"path":image_path}}),
    )
    .await;
    let upload: Value =
        serde_json::from_str(uploaded["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let validation = rpc(
        &mut input,
        &mut output,
        16,
        "tools/call",
        json!({"name":"validate_recipe","arguments":{
            "title":"Validated MCP dish",
            "description":"Dry-run candidate",
            "servings":2,
            "tags":["stdio","validation"],
            "notes":"Use a hot pan.",
            "source_url":"https://example.test/recipe",
            "image_ref":upload["ref"],
            "body":"Add @tomato{100%g}.",
            "target_servings":4
        }}),
    )
    .await;
    assert_ne!(validation["result"]["isError"], true);
    let validation: Value =
        serde_json::from_str(validation["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(validation["print_fit"].is_object());
    assert!(validation["ingredients"].is_array());

    let invalid = rpc(
        &mut input,
        &mut output,
        17,
        "tools/call",
        json!({"name":"save_draft","arguments":{
            "document":{"title":"Invalid MCP body","servings":1,"body":"First step\n~rest{}"}
        }}),
    )
    .await;
    assert_ne!(invalid["result"]["isError"], true);
    let invalid_draft: Value =
        serde_json::from_str(invalid["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let invalid = rpc(&mut input,&mut output,42,"tools/call",json!({"name":"publish_draft","arguments":{"id":invalid_draft["id"],"expected_revision":invalid_draft["revision"]}})).await;
    assert_eq!(invalid["result"]["isError"], true);
    let invalid_error = invalid["result"]["content"][0]["text"].as_str().unwrap();
    let error_json = invalid_error.split_once(": ").unwrap().1;
    let error: Value = serde_json::from_str(error_json).unwrap();
    assert_eq!(error["code"], "cooklang_invalid");
    assert_eq!(error["diagnostics"][0]["line"], 2);
    assert!(error["diagnostics"][0]["column"].as_u64().is_some());

    let unsupported_filter = rpc(
        &mut input,
        &mut output,
        18,
        "tools/call",
        json!({"name":"search_recipes","arguments":{"query":{"not_a_filter":["silently dropped"]}}}),
    )
    .await;
    assert_eq!(unsupported_filter["result"]["isError"], true);
    assert!(unsupported_filter["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("unsupported search filter"));

    #[cfg(unix)]
    {
        let special_file = rpc(
            &mut input,
            &mut output,
            19,
            "tools/call",
            json!({"name":"upload_image","arguments":{"path":"/dev/zero"}}),
        );
        let special_file = tokio::time::timeout(Duration::from_secs(2), special_file)
            .await
            .expect("special-file upload must not block");
        assert_eq!(special_file["result"]["isError"], true);
        assert!(special_file["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not a regular file"));
    }
    let oversized_path = temp.path().join("oversized.png");
    std::fs::File::create(&oversized_path)
        .unwrap()
        .set_len(10 * 1024 * 1024 + 1)
        .unwrap();
    let oversized = rpc(
        &mut input,
        &mut output,
        34,
        "tools/call",
        json!({"name":"upload_image","arguments":{"path":oversized_path}}),
    )
    .await;
    assert_eq!(oversized["result"]["isError"], true);
    assert!(oversized["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("image exceeds API 10 MiB limit"));
    let attached = rpc(
        &mut input,
        &mut output,
        6,
        "tools/call",
        json!({"name":"attach_image","arguments":{
            "id":id,"base_revision":revision,"image_ref":upload["ref"]
        }}),
    )
    .await;
    let _attached: Value =
        serde_json::from_str(attached["result"]["content"][0]["text"].as_str().unwrap()).unwrap();

    let update = rpc(&mut input,&mut output,7,"tools/call",json!({"name":"save_draft","arguments":{"recipe_id":id,"document":{"title":"MCP updated","servings":2,"body":"Add @tomato{100%g}.","tags":["stdio"]}}})).await;
    assert_ne!(update["result"]["isError"], true);
    let update: Value =
        serde_json::from_str(update["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let update = rpc(&mut input,&mut output,8,"tools/call",json!({"name":"publish_draft","arguments":{"id":update["id"],"expected_revision":update["revision"]}})).await;
    assert_ne!(update["result"]["isError"], true);

    let ingredients = rpc(
        &mut input,
        &mut output,
        9,
        "tools/call",
        json!({"name":"list_ingredients","arguments":{"q":"tomato"}}),
    )
    .await;
    let list: Value = serde_json::from_str(
        ingredients["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let ingredient_id = list[0]["id"].as_str().unwrap();
    rpc(
        &mut input,
        &mut output,
        10,
        "tools/call",
        json!({"name":"add_ingredient_alias","arguments":{"id":ingredient_id,"alias":"tomatoes"}}),
    )
    .await;
    let search = rpc(&mut input, &mut output, 11, "tools/call", json!({"name":"search_recipes","arguments":{"query":{
        "q":"MCP","ingredients":"tomatoes","ingredient_mode":"any","exclude_ingredients":"garlic",
        "tags":"stdio","tag_mode":"all","exclude_tags":"dessert","min_servings":1,"max_servings":4,
        "has_image":true,"include_deleted":false,"updated_since":"2000-01-01T00:00:00Z",
        "sort":"title","limit":20,"offset":0
    }}})).await;
    let results: Value =
        serde_json::from_str(search["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(results[0]["id"], id);

    let alias_removal = rpc(
        &mut input,
        &mut output,
        20,
        "tools/call",
        json!({"name":"add_ingredient_alias","arguments":{"id":ingredient_id,"alias":"tomatoes with space"}}),
    )
    .await;
    assert_ne!(alias_removal["result"]["isError"], true);
    let alias_removal = rpc(
        &mut input,
        &mut output,
        21,
        "tools/call",
        json!({"name":"remove_ingredient_alias","arguments":{"id":ingredient_id,"alias":"tomatoes with space"}}),
    )
    .await;
    assert_ne!(alias_removal["result"]["isError"], true);

    let rename = rpc(
        &mut input,
        &mut output,
        22,
        "tools/call",
        json!({"name":"rename_ingredient","arguments":{"id":ingredient_id,"name":"tomato renamed"}}),
    )
    .await;
    assert_ne!(rename["result"]["isError"], true);

    let second_recipe = rpc(
        &mut input,
        &mut output,
        23,
        "tools/call",
        json!({"name":"save_draft","arguments":{
            "document":{"title":"Garlic side","servings":1,"body":"Use @garlic{1%clove}."}
        }}),
    )
    .await;
    let second_draft: Value = serde_json::from_str(
        second_recipe["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let _second_recipe = rpc(&mut input,&mut output,43,"tools/call",json!({"name":"publish_draft","arguments":{"id":second_draft["id"],"expected_revision":second_draft["revision"]}})).await;
    let garlic = rpc(
        &mut input,
        &mut output,
        24,
        "tools/call",
        json!({"name":"list_ingredients","arguments":{"q":"garlic"}}),
    )
    .await;
    let garlic: Value =
        serde_json::from_str(garlic["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let merge = rpc(
        &mut input,
        &mut output,
        25,
        "tools/call",
        json!({"name":"merge_ingredients","arguments":{
            "id":garlic[0]["id"],"survivor":ingredient_id
        }}),
    )
    .await;
    assert_ne!(merge["result"]["isError"], true);

    let revision_list = rpc(
        &mut input,
        &mut output,
        26,
        "tools/call",
        json!({"name":"list_revisions","arguments":{"id":id}}),
    )
    .await;
    let revisions: Value = serde_json::from_str(
        revision_list["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(revisions.as_array().unwrap().len() >= 3);
    let first_revision = recipe["revision"].as_str().unwrap();
    let historical = rpc(
        &mut input,
        &mut output,
        27,
        "tools/call",
        json!({"name":"get_revision","arguments":{"id":id,"revision":first_revision}}),
    )
    .await;
    let historical: Value =
        serde_json::from_str(historical["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(historical["revision"], first_revision);

    let detachment = rpc(
        &mut input,
        &mut output,
        28,
        "tools/call",
        json!({"name":"detach_image","arguments":{
            "id":id,"base_revision":historical["revision"]
        }}),
    )
    .await;
    assert_eq!(detachment["result"]["isError"], true);
    let current = rpc(
        &mut input,
        &mut output,
        29,
        "tools/call",
        json!({"name":"get_recipe","arguments":{"id":id}}),
    )
    .await;
    let current: Value =
        serde_json::from_str(current["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let detachment = rpc(
        &mut input,
        &mut output,
        30,
        "tools/call",
        json!({"name":"detach_image","arguments":{
            "id":id,"base_revision":current["revision"]
        }}),
    )
    .await;
    assert_ne!(detachment["result"]["isError"], true);
    let detached: Value =
        serde_json::from_str(detachment["result"]["content"][0]["text"].as_str().unwrap()).unwrap();

    let restored_revision = rpc(
        &mut input,
        &mut output,
        31,
        "tools/call",
        json!({"name":"restore_revision","arguments":{
            "id":id,"revision":first_revision,"base_revision":detached["revision"]
        }}),
    )
    .await;
    assert_ne!(restored_revision["result"]["isError"], true);
    let restored_revision: Value = serde_json::from_str(
        restored_revision["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let deleted = rpc(
        &mut input,
        &mut output,
        32,
        "tools/call",
        json!({"name":"delete_recipe","arguments":{
            "id":id,"base_revision":restored_revision["revision"]
        }}),
    )
    .await;
    assert_ne!(deleted["result"]["isError"], true);
    let deleted: Value =
        serde_json::from_str(deleted["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let restored = rpc(
        &mut input,
        &mut output,
        33,
        "tools/call",
        json!({"name":"restore_recipe","arguments":{
            "id":id,"base_revision":deleted["revision"]
        }}),
    )
    .await;
    assert_ne!(restored["result"]["isError"], true);

    child.kill().await.unwrap();
    server.abort();
}
