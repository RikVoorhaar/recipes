#[allow(dead_code)]
mod support;

use axum::{
    body::{Body, Bytes},
    extract::Json,
    response::IntoResponse,
    routing::post,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::stream;
use image::{ImageBuffer, ImageFormat, Rgb};
use recipe_app::cover::{Generator, DEFAULT_MODEL, DEFAULT_PROMPT};
use recipe_app::storage::ImageStorage;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::{
    net::TcpListener,
    sync::{Notify, Semaphore},
};

#[derive(Clone)]
struct ProviderGate {
    block: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Semaphore>,
}

async fn fake_provider() -> (
    String,
    Arc<AtomicUsize>,
    Arc<std::sync::atomic::AtomicBool>,
    ProviderGate,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let should_fail = fail.clone();
    let gate = ProviderGate {
        block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        entered: Arc::new(Notify::new()),
        release: Arc::new(Semaphore::new(0)),
    };
    let handler_gate = gate.clone();
    let mut png = Cursor::new(Vec::new());
    ImageBuffer::from_pixel(2, 2, Rgb::<u8>([180, 80, 20]))
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let encoded = STANDARD.encode(png.into_inner());
    let app = Router::new().route(
        "/api/v1/images",
        post(move |Json(body): Json<Value>| {
            let count = count.clone();
            let should_fail = should_fail.clone();
            let gate = handler_gate.clone();
            let encoded = encoded.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                if gate.block.load(Ordering::SeqCst) {
                    gate.entered.notify_one();
                    gate.release.acquire().await.unwrap().forget();
                }
                if should_fail.load(Ordering::SeqCst) {
                    return (axum::http::StatusCode::BAD_GATEWAY, "provider unavailable")
                        .into_response();
                }
                assert_eq!(body["model"], DEFAULT_MODEL);
                assert!(body["prompt"].as_str().unwrap().contains(DEFAULT_PROMPT));
                assert!(body["prompt"]
                    .as_str()
                    .unwrap()
                    .contains("Dish: Tomato soup."));
                Json(json!({"data":[{"b64_json":encoded}]})).into_response()
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}/api/v1/images"), calls, fail, gate)
}

fn generator(endpoint: &str) -> Generator {
    Generator::with_test_endpoint("test-provider-key".into(), None, None, endpoint).unwrap()
}

fn bearer(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.bearer_auth("test-secret")
}

#[derive(Clone, Default)]
struct MemoryStorage {
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    keys: Arc<Mutex<Vec<String>>>,
    fail_put: bool,
}

impl ImageStorage for MemoryStorage {
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.keys.lock().unwrap().push(key.to_owned());
        if self.fail_put {
            return Err(io::Error::other("injected storage failure"));
        }
        self.objects
            .lock()
            .unwrap()
            .insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }
    fn get(&self, key: &str) -> io::Result<Vec<u8>> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn exists(&self, key: &str) -> io::Result<bool> {
        Ok(self.objects.lock().unwrap().contains_key(key))
    }
    fn delete(&self, key: &str) -> io::Result<()> {
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
}

#[tokio::test]
async fn draft_candidate_is_private_revision_bound_and_publishable() {
    let (endpoint, calls, _, _) = fake_provider().await;
    let app = support::TestApp::start_with_generator(generator(&endpoint)).await;
    app.configure_second_token().await;
    let draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Soup","servings":2,"body":"@tomato{200%g}"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = draft["id"].as_str().unwrap();
    let generated: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/cover"))))
            .json(&json!({"expected_revision":1,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let reference = generated["image_ref"].as_str().unwrap();
    assert_eq!(
        generated["preview_url"],
        format!("/api/v1/drafts/{id}/covers/{reference}")
    );
    assert_eq!(
        bearer(reqwest::Client::new().get(app.url(generated["preview_url"].as_str().unwrap())))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        reqwest::Client::new()
            .get(app.url(generated["preview_url"].as_str().unwrap()))
            .bearer_auth("other-secret")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let public_image = app.get(&format!("/images/{reference}")).await;
    assert_eq!(public_image.status(), reqwest::StatusCode::NOT_FOUND);
    let other_draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Other soup","servings":2,"body":"@tomato{200%g}"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cross_select = bearer(reqwest::Client::new().post(app.url(&format!(
        "/api/v1/drafts/{}/publish",
        other_draft["id"].as_str().unwrap()
    ))))
    .json(&json!({"expected_revision":1,"image_ref":reference}))
    .send()
    .await
    .unwrap();
    assert_eq!(cross_select.status(), reqwest::StatusCode::NOT_FOUND);
    let revised: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"id":id,"expected_revision":1,"document":{"description":"Revised"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(revised["revision"], 2);
    assert_eq!(
        bearer(reqwest::Client::new().get(app.url(generated["preview_url"].as_str().unwrap())))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let stale_selection =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/publish"))))
            .json(&json!({"expected_revision":2,"image_ref":reference}))
            .send()
            .await
            .unwrap();
    assert_eq!(stale_selection.status(), reqwest::StatusCode::NOT_FOUND);
    let generated: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/cover"))))
            .json(&json!({"expected_revision":2,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let reference = generated["image_ref"].as_str().unwrap();
    let published: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/publish"))))
            .json(&json!({"expected_revision":2,"image_ref":reference}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(published["recipe"]["image"]["ref"], reference);
    assert_eq!(
        app.get(published["recipe"]["image"]["url"].as_str().unwrap())
            .await
            .status(),
        reqwest::StatusCode::OK
    );
}

#[tokio::test]
async fn published_preflight_and_failed_replacement_preserve_cover() {
    let (endpoint, calls, fail, _) = fake_provider().await;
    let app = support::TestApp::start_with_generator(generator(&endpoint)).await;
    let recipe: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({"title":"Soup","servings":2,"body":"@tomato{200%g}"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let recipe = recipe["recipe"].clone();
    let id = recipe["id"].as_str().unwrap();
    let base = recipe["revision"].as_str().unwrap();
    let mut png = Cursor::new(Vec::new());
    ImageBuffer::from_pixel(2, 2, Rgb::<u8>([10, 20, 30]))
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let uploaded: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/images")))
        .header("content-type", "image/png")
        .body(png.into_inner())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let attached: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/image"))))
            .json(&json!({"base_revision":base,"image_ref":uploaded["ref"]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let current_revision = attached["revision"].as_str().unwrap();
    let denied =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/cover"))))
            .json(&json!({"base_revision":current_revision,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let stale =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/cover"))))
            .json(&json!({"base_revision":base,"dish_description":"Tomato soup.","replace":true}))
            .send()
            .await
            .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    fail.store(true, Ordering::SeqCst);
    let failed = bearer(
        reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/cover"))),
    )
    .json(
        &json!({"base_revision":current_revision,"dish_description":"Tomato soup.","replace":true}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!failed
        .text()
        .await
        .unwrap()
        .contains("provider unavailable"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let unchanged: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(unchanged["image"]["ref"], uploaded["ref"]);
    fail.store(false, Ordering::SeqCst);
    let current_revision = unchanged["revision"].as_str().unwrap();
    let replaced: Value = bearer(
        reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/cover"))),
    )
    .json(
        &json!({"base_revision":current_revision,"dish_description":"Tomato soup.","replace":true}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_ne!(replaced["image"]["ref"], uploaded["ref"]);
    assert_eq!(
        app.get(replaced["image"]["url"].as_str().unwrap())
            .await
            .status(),
        reqwest::StatusCode::OK
    );
}

#[tokio::test]
async fn discarded_candidate_remains_unpublished_in_shared_image_storage() {
    let (endpoint, _, _, _) = fake_provider().await;
    let app = support::TestApp::start_with_generator(generator(&endpoint)).await;
    let draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Soup"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = draft["id"].as_str().unwrap();
    let candidate: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/cover"))))
            .json(&json!({"expected_revision":1,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let discarded = bearer(reqwest::Client::new().delete(app.url(&format!("/api/v1/drafts/{id}"))))
        .json(&json!({"expected_revision":1}))
        .send()
        .await
        .unwrap();
    assert_eq!(discarded.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!(
            "/images/{}",
            candidate["image_ref"].as_str().unwrap()
        ))
        .await
        .status(),
        reqwest::StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn failed_candidate_binding_keeps_written_draft_image_private_and_unattachable() {
    let (endpoint, _, _, _) = fake_provider().await;
    let storage = MemoryStorage::default();
    let app = support::TestApp::start_with_generator_and_storage(
        generator(&endpoint),
        Arc::new(storage.clone()),
    )
    .await;
    app.fail_candidate_binding_writes().await;
    let draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Soup"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = draft["id"].as_str().unwrap();
    let failed =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/cover"))))
            .json(&json!({"expected_revision":1,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let reference = storage.keys.lock().unwrap().last().unwrap().clone();
    assert!(reference.starts_with("dimg_"));
    assert!(app.image_ref_is_private(&reference).await);
    assert_eq!(
        app.get(&format!("/images/{reference}")).await.status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let recipe: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({"title":"Soup","servings":2,"body":"@tomato{200%g}"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let recipe_id = recipe["recipe"]["id"].as_str().unwrap();
    let base = recipe["recipe"]["revision"].as_str().unwrap();
    let attached =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{recipe_id}/image"))))
            .json(&json!({"base_revision":base,"image_ref":reference}))
            .send()
            .await
            .unwrap();
    assert_eq!(attached.status(), reqwest::StatusCode::NOT_FOUND);
    let validated = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes/validate")))
        .json(&json!({"title":"Soup","servings":2,"body":"@tomato{200%g}","image_ref":reference}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        validated.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn storage_write_failure_leaves_a_durable_private_guard() {
    let (endpoint, _, _, _) = fake_provider().await;
    let storage = MemoryStorage {
        fail_put: true,
        ..MemoryStorage::default()
    };
    let app = support::TestApp::start_with_generator_and_storage(
        generator(&endpoint),
        Arc::new(storage.clone()),
    )
    .await;
    let draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Soup"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = draft["id"].as_str().unwrap();
    let failed =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/cover"))))
            .json(&json!({"expected_revision":1,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let reference = storage.keys.lock().unwrap().last().unwrap().clone();
    assert!(app.image_ref_is_private(&reference).await);
    assert_eq!(
        app.get(&format!("/images/{reference}")).await.status(),
        reqwest::StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn failed_publish_does_not_promote_private_candidate() {
    let (endpoint, _, _, _) = fake_provider().await;
    let app = support::TestApp::start_with_generator(generator(&endpoint)).await;
    let draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Soup","servings":2,"body":"@tomato{200%g}"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = draft["id"].as_str().unwrap();
    let candidate: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/cover"))))
            .json(&json!({"expected_revision":1,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let reference = candidate["image_ref"].as_str().unwrap();
    app.fail_recipe_revision_writes().await;
    let failed =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/drafts/{id}/publish"))))
            .json(&json!({"expected_revision":1,"image_ref":reference}))
            .send()
            .await
            .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert!(app.image_ref_is_private(reference).await);
    assert_eq!(
        app.get(&format!("/images/{reference}")).await.status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let still_draft = bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/drafts/{id}"))))
        .send()
        .await
        .unwrap();
    assert_eq!(still_draft.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn in_flight_draft_generation_rechecks_revision_after_provider_returns() {
    let (endpoint, calls, _, gate) = fake_provider().await;
    let app = support::TestApp::start_with_generator(generator(&endpoint)).await;
    let draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Soup"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = draft["id"].as_str().unwrap().to_owned();
    gate.block.store(true, Ordering::SeqCst);
    let url = app.url(&format!("/api/v1/drafts/{id}/cover"));
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url)
            .bearer_auth("test-secret")
            .json(&json!({"expected_revision":1,"dish_description":"Tomato soup."}))
            .send()
            .await
            .unwrap()
    });
    gate.entered.notified().await;
    let revised = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"id":id,"expected_revision":1,"document":{"description":"Changed while generating"}}))
        .send().await.unwrap();
    assert_eq!(revised.status(), reqwest::StatusCode::OK);
    gate.release.add_permits(1);
    assert_eq!(
        request.await.unwrap().status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn in_flight_published_replacement_conflict_preserves_current_cover() {
    let (endpoint, calls, _, gate) = fake_provider().await;
    let storage = MemoryStorage::default();
    let app = support::TestApp::start_with_generator_and_storage(
        generator(&endpoint),
        Arc::new(storage.clone()),
    )
    .await;
    let created: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/recipes")))
        .json(&json!({"title":"Soup","servings":2,"body":"@tomato{200%g}"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let recipe = created["recipe"].clone();
    let id = recipe["id"].as_str().unwrap().to_owned();
    let base = recipe["revision"].as_str().unwrap().to_owned();
    let png = {
        let mut buffer = Cursor::new(Vec::new());
        ImageBuffer::from_pixel(2, 2, Rgb::<u8>([10, 20, 30]))
            .write_to(&mut buffer, ImageFormat::Png)
            .unwrap();
        buffer.into_inner()
    };
    let uploaded: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/images")))
        .header("content-type", "image/png")
        .body(png)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let attached: Value =
        bearer(reqwest::Client::new().post(app.url(&format!("/api/v1/recipes/{id}/image"))))
            .json(&json!({"base_revision":base,"image_ref":uploaded["ref"]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let current_base = attached["revision"].as_str().unwrap().to_owned();
    gate.block.store(true, Ordering::SeqCst);
    let url = app.url(&format!("/api/v1/recipes/{id}/cover"));
    let generation_base = current_base.clone();
    let generating = tokio::spawn(async move {
        reqwest::Client::new().post(url).bearer_auth("test-secret")
            .json(&json!({"base_revision":generation_base,"dish_description":"Tomato soup.","replace":true}))
            .send().await.unwrap()
    });
    gate.entered.notified().await;
    let edited: Value = bearer(reqwest::Client::new().put(app.url(&format!("/api/v1/recipes/{id}"))))
        .json(&json!({"title":"Changed soup","servings":2,"body":"@tomato{200%g}","base_revision":current_base}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(edited["recipe"]["image"]["ref"], uploaded["ref"]);
    gate.release.add_permits(1);
    assert_eq!(
        generating.await.unwrap().status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let orphan_ref = storage.keys.lock().unwrap().last().unwrap().clone();
    assert!(orphan_ref.starts_with("dimg_"));
    assert_eq!(
        app.get(&format!("/images/{orphan_ref}")).await.status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let current: Value =
        bearer(reqwest::Client::new().get(app.url(&format!("/api/v1/recipes/{id}"))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(current["image"]["ref"], uploaded["ref"]);
}

#[tokio::test]
async fn model_prompt_overrides_and_unicode_scalar_limit_are_enforced() {
    let mut png = Cursor::new(Vec::new());
    ImageBuffer::from_pixel(2, 2, Rgb::<u8>([1, 2, 3]))
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let encoded = STANDARD.encode(png.into_inner());
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(None));
    let call_count = calls.clone();
    let seen_body = seen.clone();
    let encoded_body = encoded.clone();
    let app = Router::new().route(
        "/api/v1/images",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                let call_count = call_count.clone();
                let seen_body = seen_body.clone();
                let encoded = encoded_body.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer local-test-key");
                    call_count.fetch_add(1, Ordering::SeqCst);
                    *seen_body.lock().unwrap() = Some(body);
                    Json(json!({"data":[{"b64_json":encoded}]}))
                }
            },
        ),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoint = format!("http://{address}/api/v1/images");
    let generator = Generator::with_test_endpoint(
        "local-test-key".into(),
        Some("test/model-override".into()),
        Some("Operator house prompt".into()),
        &endpoint,
    )
    .unwrap();
    let dish = "é".repeat(500);
    generator.generate(&dish).await.unwrap();
    let body = seen.lock().unwrap().clone().unwrap();
    assert_eq!(body["model"], "test/model-override");
    assert_eq!(
        body["prompt"],
        format!("Operator house prompt\n\nDish: {dish}")
    );
    assert!(generator.generate(&"é".repeat(501)).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn malformed_and_oversized_provider_responses_are_rejected_with_one_call() {
    for (body, expected) in [
        (
            br#"{"data":[{"b64_json":"%%%"}]}"#.to_vec(),
            "invalid image data",
        ),
        (vec![b'x'; 20 * 1024 * 1024 + 1], "exceeds size limit"),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let response_body = body.clone();
        let app = Router::new().route(
            "/api/v1/images",
            post(move || {
                let count = count.clone();
                let body = response_body.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let chunks = body
                        .chunks(1024 * 1024)
                        .map(|chunk| Ok::<_, io::Error>(Bytes::copy_from_slice(chunk)))
                        .collect::<Vec<_>>();
                    axum::http::Response::builder()
                        .status(axum::http::StatusCode::OK)
                        .body(Body::from_stream(stream::iter(chunks)))
                        .unwrap()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let generator = generator(&format!("http://{address}/api/v1/images"));
        let error = generator.generate("Tomato soup.").await.unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn missing_invalid_configuration_and_redirects_never_retry_or_follow() {
    assert!(Generator::new("".into(), None, None).is_err());
    assert!(Generator::new("key".into(), None, Some("  ".into())).is_err());
    assert!(
        Generator::with_test_endpoint("key".into(), None, None, "https://127.0.0.1/x").is_err()
    );
    let app = support::TestApp::start().await;
    let draft: Value = bearer(reqwest::Client::new().post(app.url("/api/v1/drafts")))
        .json(&json!({"document":{"title":"Soup"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let unavailable = bearer(reqwest::Client::new().post(app.url(&format!(
        "/api/v1/drafts/{}/cover",
        draft["id"].as_str().unwrap()
    ))))
    .json(&json!({"expected_revision":1,"dish_description":"Tomato soup."}))
    .send()
    .await
    .unwrap();
    assert_eq!(
        unavailable.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );

    let redirect_target_calls = Arc::new(AtomicUsize::new(0));
    let target_count = redirect_target_calls.clone();
    let app = Router::new()
        .route(
            "/api/v1/images",
            post(|| async {
                (
                    axum::http::StatusCode::TEMPORARY_REDIRECT,
                    [(axum::http::header::LOCATION, "/target")],
                )
            }),
        )
        .route(
            "/target",
            post(move || {
                let count = target_count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    "should not be reached"
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let redirecting = generator(&format!("http://{address}/api/v1/images"));
    assert!(redirecting.generate("Tomato soup.").await.is_err());
    assert_eq!(redirect_target_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn test_endpoint_cannot_target_non_loopback() {
    assert!(
        Generator::with_test_endpoint("x".into(), None, None, "http://example.com/image").is_err()
    );
}
