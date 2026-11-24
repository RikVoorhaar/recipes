#[allow(dead_code)]
mod support;

use image::{DynamicImage, GenericImageView, ImageFormat, Rgb};
use serde_json::{json, Value};
use std::io::Cursor;
use support::TestApp;

fn png() -> Vec<u8> {
    png_sized(1800, 900)
}

fn png_sized(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        width,
        height,
        Rgb([220, 30, 40]),
    ))
    .write_to(&mut bytes, ImageFormat::Png)
    .unwrap();
    bytes.into_inner()
}

async fn recipe(app: &TestApp) -> Value {
    app.client()
        .post(app.url("/api/v1/recipes"))
        .bearer_auth("test-secret")
        .json(&json!({"title":"Soup","servings":2,"body":"@salt{1%g}"}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["recipe"]
        .clone()
}

#[tokio::test]
async fn upload_attach_detach_retains_images_and_searches_current_attachment() {
    let app = TestApp::start().await;
    let mut initial = recipe(&app).await;
    let ingredients: Value = app
        .client()
        .get(app.url("/api/v1/ingredients"))
        .bearer_auth("test-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let salt_id = ingredients[0]["id"].as_str().unwrap();
    assert_eq!(ingredients[0]["name"], "salt");
    assert_eq!(
        app.client()
            .post(app.url(&format!("/api/v1/ingredients/{salt_id}/aliases")))
            .bearer_auth("test-secret")
            .json(&json!({"alias":"salty"}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    let changed: Value = app
        .client()
        .put(app.url(&format!(
            "/api/v1/recipes/{}",
            initial["id"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({
            "title":"Soup",
            "servings":2,
            "body":"@salty{1%g}",
            "base_revision":initial["revision"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    initial = changed["recipe"].clone();
    assert_eq!(
        app.client()
            .delete(app.url(&format!("/api/v1/ingredients/{salt_id}/aliases/salty")))
            .bearer_auth("test-secret")
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let uploaded = app
        .client()
        .post(app.url("/api/v1/images"))
        .bearer_auth("test-secret")
        .header("content-type", "image/png")
        .body(png())
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201);
    let image: Value = uploaded.json().await.unwrap();
    let reference = image["ref"].as_str().unwrap();
    assert!(reference.starts_with("img_"));

    let attached: Value = app
        .client()
        .post(app.url(&format!(
            "/api/v1/recipes/{}/image",
            initial["id"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({"base_revision":initial["revision"],"image_ref":reference}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(attached["image"]["ref"], reference);
    let old = app.get(image["url"].as_str().unwrap()).await;
    assert_eq!(old.status(), 200);
    assert_eq!(old.headers()["content-type"], "image/jpeg");
    let normalized = old.bytes().await.unwrap();
    assert_eq!(&normalized[..2], &[0xff, 0xd8]);
    let dimensions = image::load_from_memory(&normalized).unwrap().dimensions();
    assert_eq!(dimensions, (1600, 800));
    let canonical_search: Value = app
        .client()
        .get(app.url(&format!("/api/v1/search?ingredients={salt_id}")))
        .bearer_auth("test-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(canonical_search.as_array().unwrap().len(), 1);
    let yes: Value = app
        .client()
        .get(app.url("/api/v1/search?has_image=true"))
        .bearer_auth("test-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(yes.as_array().unwrap().len(), 1);
    let no: Value = app
        .client()
        .get(app.url("/api/v1/search?has_image=false"))
        .bearer_auth("test-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(no.as_array().unwrap().is_empty());

    let detached: Value = app
        .client()
        .delete(app.url(&format!(
            "/api/v1/recipes/{}/image",
            initial["id"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({"base_revision":attached["revision"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(detached["image"].is_null());
    assert_eq!(
        app.get(image["url"].as_str().unwrap())
            .await
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        normalized.as_ref()
    );
    let historical: Value = app
        .client()
        .get(app.url(&format!(
            "/api/v1/recipes/{}/revisions/{}",
            initial["id"].as_str().unwrap(),
            attached["revision"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(historical["image"]["ref"], reference);
    let restored: Value = app
        .client()
        .post(app.url(&format!(
            "/api/v1/recipes/{}/revisions/{}",
            initial["id"].as_str().unwrap(),
            attached["revision"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({"base_revision":detached["revision"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restored["image"]["ref"], reference);

    let replacement: Value = app
        .client()
        .post(app.url("/api/v1/images"))
        .bearer_auth("test-secret")
        .header("content-type", "image/png")
        .body(png())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let replaced: Value = app
        .client()
        .post(app.url(&format!(
            "/api/v1/recipes/{}/image",
            initial["id"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({
            "base_revision": restored["revision"],
            "image_ref": replacement["ref"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replaced["image"]["ref"], replacement["ref"]);
    assert_eq!(
        app.get(image["url"].as_str().unwrap())
            .await
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        normalized.as_ref()
    );

    let deleted: Value = app
        .client()
        .delete(app.url(&format!(
            "/api/v1/recipes/{}",
            initial["id"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({"base_revision":replaced["revision"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(deleted["deleted"].as_bool().unwrap());
    let restored_after_delete: Value = app
        .client()
        .post(app.url(&format!(
            "/api/v1/recipes/{}/restore",
            initial["id"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({"base_revision":deleted["revision"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restored_after_delete["image"]["ref"], replacement["ref"]);
    assert_eq!(
        app.get(replacement["url"].as_str().unwrap()).await.status(),
        200
    );
}

#[tokio::test]
async fn rejects_unauthorized_unsupported_and_undecodable_uploads() {
    let app = TestApp::start().await;
    assert_eq!(
        app.client()
            .post(app.url("/api/v1/images"))
            .body(png())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        app.client()
            .post(app.url("/api/v1/images"))
            .bearer_auth("test-secret")
            .header("content-type", "image/gif")
            .body(png())
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    assert_eq!(
        app.client()
            .post(app.url("/api/v1/images"))
            .bearer_auth("test-secret")
            .header("content-type", "image/jpeg")
            .body(png())
            .send()
            .await
            .unwrap()
            .status(),
        422,
        "declared MIME must match decoded file format"
    );
    assert_eq!(
        app.client()
            .post(app.url("/api/v1/images"))
            .bearer_auth("test-secret")
            .header("content-type", "image/png")
            .body(&b"not an image"[..])
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let oversized = app
        .client()
        .post(app.url("/api/v1/images"))
        .bearer_auth("test-secret")
        .header("content-type", "image/png")
        .body(vec![0; 10 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);
    let error: Value = oversized.json().await.unwrap();
    assert_eq!(error["code"], "payload_too_large");
    assert_eq!(error["message"], "Image upload exceeds the 10 MiB limit");
}

#[tokio::test]
async fn accepts_exact_sixteen_megapixel_boundary() {
    let app = TestApp::start().await;
    let uploaded = app
        .client()
        .post(app.url("/api/v1/images"))
        .bearer_auth("test-secret")
        .header("content-type", "image/png")
        .body(png_sized(4000, 4000))
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201);
    let image: Value = uploaded.json().await.unwrap();
    assert!(image["ref"].as_str().unwrap().starts_with("img_"));
    let normalized = app.get(image["url"].as_str().unwrap()).await;
    assert_eq!(normalized.status(), 200);
    assert_eq!(
        image::load_from_memory(&normalized.bytes().await.unwrap())
            .unwrap()
            .dimensions(),
        (1600, 1600)
    );
}

#[tokio::test]
async fn stale_unknown_bases_conflict_and_competing_attachment_is_atomic() {
    let app = TestApp::start().await;
    let current = recipe(&app).await;
    let mut uploads = Vec::new();
    for _ in 0..2 {
        uploads.push(
            app.client()
                .post(app.url("/api/v1/images"))
                .bearer_auth("test-secret")
                .header("content-type", "image/png")
                .body(png())
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap(),
        );
    }

    let endpoint = app.url(&format!(
        "/api/v1/recipes/{}/image",
        current["id"].as_str().unwrap()
    ));
    let attach_one = app
        .client()
        .post(&endpoint)
        .bearer_auth("test-secret")
        .json(&json!({
            "base_revision":current["revision"],
            "image_ref":uploads[0]["ref"]
        }))
        .send();
    let attach_two = app
        .client()
        .post(&endpoint)
        .bearer_auth("test-secret")
        .json(&json!({
            "base_revision":current["revision"],
            "image_ref":uploads[1]["ref"]
        }))
        .send();
    let (one, two) = tokio::join!(attach_one, attach_two);
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_ne!(one.status(), two.status());
    let (success, conflict) = if one.status() == 200 {
        (one, two)
    } else {
        (two, one)
    };
    assert_eq!(conflict.status(), 409);
    let success: Value = success.json().await.unwrap();
    let conflict: Value = conflict.json().await.unwrap();
    assert_eq!(conflict["code"], "conflict");
    assert_eq!(conflict["current_revision_id"], success["revision"]);

    let stale_base = "rev_unknown_but_recipe_exists";
    let stale_update = app
        .client()
        .put(app.url(&format!(
            "/api/v1/recipes/{}",
            current["id"].as_str().unwrap()
        )))
        .bearer_auth("test-secret")
        .json(&json!({
            "title":"Soup",
            "servings":2,
            "body":"@salt{1%g}",
            "base_revision":stale_base
        }))
        .send()
        .await
        .unwrap();
    let stale_attach = app
        .client()
        .post(&endpoint)
        .bearer_auth("test-secret")
        .json(&json!({
            "base_revision":stale_base,
            "image_ref":uploads[0]["ref"]
        }))
        .send()
        .await
        .unwrap();
    let stale_detach = app
        .client()
        .delete(&endpoint)
        .bearer_auth("test-secret")
        .json(&json!({"base_revision":stale_base}))
        .send()
        .await
        .unwrap();
    for response in [stale_update, stale_attach, stale_detach] {
        assert_eq!(response.status(), 409);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["code"], "conflict");
        assert_eq!(error["current_revision_id"], success["revision"]);
    }
}
