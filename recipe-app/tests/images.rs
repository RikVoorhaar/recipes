#[path = "support/mcp.rs"]
mod mcp;
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
    let client = app.client();
    let base = app.url("");
    let draft = mcp::tool(
        &client,
        &base,
        "test-secret",
        "save_draft",
        json!({
            "document":{"title":"Soup","servings":2,"body":"@salt{1%g}"}
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
    .await["recipe"]
        .clone()
}

#[tokio::test]
async fn attach_detach_retains_images_and_searches_current_attachment() {
    let app = TestApp::start().await;
    let mut initial = recipe(&app).await;
    let client = app.client();
    let base = app.url("");
    let ingredients = mcp::tool(&client, &base, "test-secret", "list_ingredients", json!({})).await;
    let salt_id = ingredients[0]["id"].as_str().unwrap();
    assert_eq!(ingredients[0]["name"], "salt");
    let added = mcp::call(
        &client,
        &base,
        "test-secret",
        "add_ingredient_alias",
        json!({"id":salt_id,"alias":"salty"}),
    )
    .await;
    assert_ne!(added["isError"], true);
    let draft = mcp::tool(
        &client,
        &base,
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":initial["id"],"document":{"body":"@salty{1%g}"}
        }),
    )
    .await;
    let changed = mcp::tool(
        &client,
        &base,
        "test-secret",
        "publish_draft",
        json!({
            "id":draft["id"],"expected_revision":draft["revision"]
        }),
    )
    .await;
    initial = changed["recipe"].clone();
    let removed = mcp::call(
        &client,
        &base,
        "test-secret",
        "remove_ingredient_alias",
        json!({"id":salt_id,"alias":"salty"}),
    )
    .await;
    assert_ne!(removed["isError"], true);
    let reference = "img_fixture";
    let image_url = format!("/images/{reference}");
    let mut jpeg = Cursor::new(Vec::new());
    image::load_from_memory(&png())
        .unwrap()
        .thumbnail(1600, 1600)
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .unwrap();
    app.install_image_fixture(reference, jpeg.get_ref());

    let attached = mcp::tool(
        &client,
        &base,
        "test-secret",
        "attach_image",
        json!({
            "id":initial["id"],"base_revision":initial["revision"],"image_ref":reference
        }),
    )
    .await;
    assert_eq!(attached["image"]["ref"], reference);
    let old = app.get(&image_url).await;
    assert_eq!(old.status(), 200);
    assert_eq!(old.headers()["content-type"], "image/jpeg");
    let normalized = old.bytes().await.unwrap();
    assert_eq!(&normalized[..2], &[0xff, 0xd8]);
    let dimensions = image::load_from_memory(&normalized).unwrap().dimensions();
    assert_eq!(dimensions, (1600, 800));
    let canonical_search = mcp::tool(
        &client,
        &base,
        "test-secret",
        "search_recipes",
        json!({"query":{"ingredients":salt_id}}),
    )
    .await;
    assert_eq!(canonical_search.as_array().unwrap().len(), 1);
    let yes = mcp::tool(
        &client,
        &base,
        "test-secret",
        "search_recipes",
        json!({"query":{"has_image":true}}),
    )
    .await;
    assert_eq!(yes.as_array().unwrap().len(), 1);
    let no = mcp::tool(
        &client,
        &base,
        "test-secret",
        "search_recipes",
        json!({"query":{"has_image":false}}),
    )
    .await;
    assert!(no.as_array().unwrap().is_empty());

    let detached = mcp::tool(
        &client,
        &base,
        "test-secret",
        "detach_image",
        json!({
            "id":initial["id"],"base_revision":attached["revision"]
        }),
    )
    .await;
    assert!(detached["image"].is_null());
    assert_eq!(
        app.get(&image_url).await.bytes().await.unwrap().as_ref(),
        normalized.as_ref()
    );
    let historical = mcp::tool(
        &client,
        &base,
        "test-secret",
        "get_revision",
        json!({
            "id":initial["id"],"revision":attached["revision"]
        }),
    )
    .await;
    assert_eq!(historical["image"]["ref"], reference);
    let restored = mcp::tool(
        &client,
        &base,
        "test-secret",
        "restore_revision",
        json!({
            "id":initial["id"],"revision":attached["revision"],"base_revision":detached["revision"]
        }),
    )
    .await;
    assert_eq!(restored["image"]["ref"], reference);

    let replacement = "img_replacement";
    app.install_image_fixture(replacement, jpeg.get_ref());
    let replaced = mcp::tool(
        &client,
        &base,
        "test-secret",
        "attach_image",
        json!({
            "id":initial["id"],"base_revision":restored["revision"],"image_ref":replacement
        }),
    )
    .await;
    assert_eq!(replaced["image"]["ref"], replacement);
    assert_eq!(
        app.get(&image_url).await.bytes().await.unwrap().as_ref(),
        normalized.as_ref()
    );

    let deleted = mcp::tool(
        &client,
        &base,
        "test-secret",
        "delete_recipe",
        json!({
            "id":initial["id"],"base_revision":replaced["revision"]
        }),
    )
    .await;
    assert!(deleted["deleted"].as_bool().unwrap());
    let restored_after_delete = mcp::tool(
        &client,
        &base,
        "test-secret",
        "restore_recipe",
        json!({
            "id":initial["id"],"base_revision":deleted["revision"]
        }),
    )
    .await;
    assert_eq!(restored_after_delete["image"]["ref"], replacement);
    assert_eq!(
        app.get(&format!("/images/{replacement}")).await.status(),
        200
    );
}

#[tokio::test]
async fn stale_unknown_bases_conflict_and_competing_attachment_is_atomic() {
    let app = TestApp::start().await;
    let current = recipe(&app).await;
    let mut jpeg = Cursor::new(Vec::new());
    image::load_from_memory(&png())
        .unwrap()
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .unwrap();
    app.install_image_fixture("img_one", jpeg.get_ref());
    app.install_image_fixture("img_two", jpeg.get_ref());
    let client = app.client();
    let endpoint = app.url("");
    let attach_one = mcp::call(
        &client,
        &endpoint,
        "test-secret",
        "attach_image",
        json!({
            "id":current["id"],"base_revision":current["revision"],"image_ref":"img_one"
        }),
    );
    let attach_two = mcp::call(
        &client,
        &endpoint,
        "test-secret",
        "attach_image",
        json!({
            "id":current["id"],"base_revision":current["revision"],"image_ref":"img_two"
        }),
    );
    let (one, two) = tokio::join!(attach_one, attach_two);
    assert_ne!(one["isError"], two["isError"]);
    let (success, conflict) = if one["isError"] != true {
        (one, two)
    } else {
        (two, one)
    };
    let success: Value =
        serde_json::from_str(success["content"][0]["text"].as_str().unwrap()).unwrap();
    let conflict: Value =
        serde_json::from_str(conflict["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(conflict["code"], "conflict");
    assert_eq!(conflict["current_revision_id"], success["revision"]);

    let stale_base = "rev_unknown_but_recipe_exists";
    let stale_attach = mcp::call(
        &client,
        &endpoint,
        "test-secret",
        "attach_image",
        json!({
            "id":current["id"],"base_revision":stale_base,"image_ref":"img_one"
        }),
    )
    .await;
    let stale_detach = mcp::call(
        &client,
        &endpoint,
        "test-secret",
        "detach_image",
        json!({
            "id":current["id"],"base_revision":stale_base
        }),
    )
    .await;
    for response in [stale_attach, stale_detach] {
        assert_eq!(response["isError"], true);
        let error: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(error["code"], "conflict");
        assert_eq!(error["current_revision_id"], success["revision"]);
    }
    let current_after = mcp::tool(
        &client,
        &endpoint,
        "test-secret",
        "get_recipe",
        json!({"id":current["id"]}),
    )
    .await;
    assert_eq!(current_after["revision"], success["revision"]);
}
