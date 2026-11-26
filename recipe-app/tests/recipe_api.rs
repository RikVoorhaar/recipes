#[path = "support/mcp.rs"]
mod mcp;
#[allow(dead_code)]
mod support;

use serde_json::{json, Value};
use std::io::Cursor;

async fn create(app: &support::TestApp) -> Value {
    publish(
        app,
        json!({"title":"Butter noodles","description":"A simple supper","servings":2,
        "tags":["quick"],"body":"@butter{20%g}\n@pasta{200%g}"}),
    )
    .await
}

async fn publish(app: &support::TestApp, document: Value) -> Value {
    let client = app.client();
    let base = app.url("");
    let draft = mcp::tool(
        &client,
        &base,
        "test-secret",
        "save_draft",
        json!({"document":document}),
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
    .await
}

async fn scaled_ingredients(app: &support::TestApp, recipe: &Value, servings: u64) -> Value {
    mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "validate_recipe",
        json!({
            "title":recipe["title"], "servings":recipe["servings"],
            "body":recipe["body"], "target_servings":servings
        }),
    )
    .await
}

async fn tool(app: &support::TestApp, name: &str, arguments: Value) -> Value {
    mcp::tool(&app.client(), &app.url(""), "test-secret", name, arguments).await
}

async fn call(app: &support::TestApp, token: &str, name: &str, arguments: Value) -> Value {
    mcp::call(&app.client(), &app.url(""), token, name, arguments).await
}

fn error(result: &Value) -> Value {
    assert_eq!(result["isError"], true, "{result}");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn drafts_are_private_inert_revision_checked_and_publish_atomically() {
    let app = support::TestApp::start().await;
    app.configure_second_token().await;
    let draft = tool(
        &app,
        "save_draft",
        json!({"document":{"title":"Private partial"}}),
    )
    .await;
    let id = draft["id"].as_str().unwrap();
    assert_eq!(draft["revision"], 1);
    assert!(!app
        .get("/")
        .await
        .text()
        .await
        .unwrap()
        .contains("Private partial"));
    let other_list = mcp::tool(
        &app.client(),
        &app.url(""),
        "other-secret",
        "list_drafts",
        json!({}),
    )
    .await;
    assert!(other_list.as_array().unwrap().is_empty());
    let public_recipes = tool(&app, "list_recipes", json!({})).await;
    assert!(public_recipes.as_array().unwrap().is_empty());
    assert_eq!(app.revision_count().await, 0);
    assert_eq!(app.ingredient_count().await, 0);
    let tags = tool(&app, "list_tags", json!({})).await;
    assert!(tags.as_array().unwrap().is_empty());
    assert!(!app
        .get("/ingredients")
        .await
        .text()
        .await
        .unwrap()
        .contains("Private partial"));

    let other = call(&app, "other-secret", "get_draft", json!({"id":id})).await;
    assert_eq!(error(&other)["code"], "not_found");
    let listed = tool(&app, "list_drafts", json!({})).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    for method in ["save", "discard", "publish"] {
        let (name, args) = match method {
            "save" => (
                "save_draft",
                json!({"id":id,"expected_revision":1,"document":{"title":"stolen"}}),
            ),
            "discard" => ("discard_draft", json!({"id":id,"expected_revision":1})),
            _ => ("publish_draft", json!({"id":id,"expected_revision":1})),
        };
        let response = call(&app, "other-secret", name, args).await;
        assert!(matches!(
            error(&response)["code"].as_str(),
            Some("not_found" | "conflict")
        ));
    }

    let draft = tool(&app, "save_draft", json!({"id":id,"expected_revision":1,"document":{"title":"Draft noodles","servings":2,"body":"~rest{}","tags":["private"]}})).await;
    let revision = draft["revision"].as_i64().unwrap();
    let stale_save = call(
        &app,
        "test-secret",
        "save_draft",
        json!({"id":id,"expected_revision":1,"document":{"title":"Overwritten"}}),
    )
    .await;
    assert_eq!(error(&stale_save)["code"], "conflict");
    let invalid = call(
        &app,
        "test-secret",
        "publish_draft",
        json!({"id":id,"expected_revision":revision}),
    )
    .await;
    assert_eq!(error(&invalid)["code"], "cooklang_invalid");
    let retained = tool(&app, "get_draft", json!({"id":id})).await;
    assert_eq!(retained["revision"], revision);

    let fixed = tool(
        &app,
        "save_draft",
        json!({"id":id,"expected_revision":revision,"document":{"body":"Add @tomato{100%g}."}}),
    )
    .await;
    let private_search = tool(
        &app,
        "search_recipes",
        json!({"query":{"q":"Draft noodles","ingredients":"tomato"}}),
    )
    .await;
    assert!(private_search.as_array().unwrap().is_empty());
    let private_ingredient = tool(&app, "list_ingredients", json!({"q":"tomato"})).await;
    assert!(private_ingredient.as_array().unwrap().is_empty());
    assert!(!app
        .get("/ingredients")
        .await
        .text()
        .await
        .unwrap()
        .contains("tomato"));
    let (left, right) = tokio::join!(
        call(
            &app,
            "test-secret",
            "publish_draft",
            json!({"id":id,"expected_revision":fixed["revision"]})
        ),
        call(
            &app,
            "test-secret",
            "publish_draft",
            json!({"id":id,"expected_revision":fixed["revision"]})
        )
    );
    assert_ne!(left["isError"], right["isError"]);
    let failed = if left["isError"] == true { left } else { right };
    assert!(matches!(
        error(&failed)["code"].as_str(),
        Some("not_found" | "conflict")
    ));
    let published = tool(&app, "list_recipes", json!({})).await;
    assert_eq!(published.as_array().unwrap().len(), 1);
    assert_eq!(published[0]["title"], "Draft noodles");
    assert_eq!(app.revision_count().await, 1);
    assert_eq!(
        app.get("/")
            .await
            .text()
            .await
            .unwrap()
            .matches("Draft noodles")
            .count(),
        1
    );
    let discarded = tool(&app, "save_draft", json!({"document":{"title":"discard"}})).await;
    assert!(discarded["id"].is_string(), "discard draft: {discarded}");
    assert_eq!(discarded["revision"], 1);
    let discarded_id = discarded["id"].as_str().unwrap();
    assert_eq!(
        tool(&app, "get_draft", json!({"id":discarded_id})).await["id"],
        discarded_id
    );
    let response = call(
        &app,
        "test-secret",
        "discard_draft",
        json!({"id":discarded_id,"expected_revision":discarded["revision"]}),
    )
    .await;
    assert_ne!(response["isError"], true);

    let recipe_id = published[0]["id"].as_str().unwrap();
    let edit = tool(
        &app,
        "save_draft",
        json!({"recipe_id":recipe_id,"document":{"title":"Stale draft edit"}}),
    )
    .await;
    let newer_draft = tool(
        &app,
        "save_draft",
        json!({"recipe_id":recipe_id,"document":{
            "title":"Newer published edit","servings":2,"body":"@tomato{200%g}"
        }}),
    )
    .await;
    let newer = tool(
        &app,
        "publish_draft",
        json!({"id":newer_draft["id"],"expected_revision":newer_draft["revision"]}),
    )
    .await;
    let stale_publish = call(
        &app,
        "test-secret",
        "publish_draft",
        json!({"id":edit["id"],"expected_revision":edit["revision"]}),
    )
    .await;
    assert_eq!(error(&stale_publish)["code"], "conflict");
    let retained = tool(&app, "get_draft", json!({"id":edit["id"]})).await;
    assert_eq!(retained["revision"], edit["revision"]);
    let history = tool(&app, "list_revisions", json!({"id":recipe_id})).await;
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert_eq!(newer["recipe"]["id"], recipe_id);
    assert_eq!(newer["recipe"]["title"], "Newer published edit");
}

#[tokio::test]
async fn publishing_an_edit_draft_preserves_recipe_identity_cover_and_history() {
    let app = support::TestApp::start().await;
    let initial = create(&app).await["recipe"].clone();
    let recipe_id = initial["id"].as_str().unwrap();
    let first_revision = initial["revision"].as_str().unwrap();
    let mut jpeg = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(12, 12, image::Rgb([120, 80, 40]))
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    app.install_image_fixture("img_edit_fixture", jpeg.get_ref());
    let attached = tool(
        &app,
        "attach_image",
        json!({"id":recipe_id,"base_revision":first_revision,"image_ref":"img_edit_fixture"}),
    )
    .await;
    let attached_revision = attached["revision"].as_str().unwrap();

    let draft = tool(
        &app,
        "save_draft",
        json!({
            "recipe_id":recipe_id,
            "document":{"title":"Drafted edit","description":"Text-only change","servings":3}
        }),
    )
    .await;
    let saved = tool(
        &app,
        "save_draft",
        json!({
            "id":draft["id"],"expected_revision":draft["revision"],
            "document":{"body":"Add @butter{30%g} and @pasta{300%g}."}
        }),
    )
    .await;
    let published = tool(
        &app,
        "publish_draft",
        json!({"id":draft["id"],"expected_revision":saved["revision"]}),
    )
    .await;

    let result = &published["recipe"];
    assert_eq!(result["id"], recipe_id);
    assert_eq!(result["title"], "Drafted edit");
    assert_eq!(result["image"]["ref"], "img_edit_fixture");
    assert_ne!(result["revision"], attached_revision);
    let history = tool(&app, "list_revisions", json!({"id":recipe_id})).await;
    assert_eq!(history.as_array().unwrap().len(), 3);
    let revisions: Vec<&str> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|revision| revision["revision"].as_str().unwrap())
        .collect();
    assert!(revisions.contains(&first_revision));
    assert!(revisions.contains(&attached_revision));
    assert!(revisions.contains(&result["revision"].as_str().unwrap()));
    let prior = tool(
        &app,
        "get_revision",
        json!({"id":recipe_id,"revision":attached_revision}),
    )
    .await;
    assert_eq!(prior["image"]["ref"], "img_edit_fixture");
    assert_eq!(prior["title"], "Butter noodles");
}

#[tokio::test]
async fn edit_draft_publishes_over_pool_capacity_without_internal_errors() {
    let app = support::TestApp::start().await;
    const RECIPE_COUNT: usize = 12;
    let client = reqwest::Client::new();
    let mut publish_requests = Vec::new();
    for index in 0..RECIPE_COUNT {
        let created = publish(
            &app,
            json!({
                "title":format!("Contention recipe {index}"),
                "servings":2,
                "body":"Add @rice{100%g}."
            }),
        )
        .await;
        let recipe_id = created["recipe"]["id"].as_str().unwrap();
        let draft = tool(
            &app,
            "save_draft",
            json!({
                "recipe_id":recipe_id,
                "document":{"title":format!("Contention edit {index}")}
            }),
        )
        .await;
        publish_requests.push((
            draft["id"].as_str().unwrap().to_owned(),
            draft["revision"].as_i64().unwrap(),
        ));
    }
    // SQLx's default SQLite pool capacity is 10; these independent transactions
    // contend concurrently while each publishes an edit on a distinct recipe.
    let mut tasks = tokio::task::JoinSet::new();
    for (id, revision) in &publish_requests {
        let client = client.clone();
        let url = app.url("");
        let id = id.clone();
        let revision = *revision;
        tasks.spawn(async move {
            mcp::call(
                &client,
                &url,
                "test-secret",
                "publish_draft",
                json!({
                    "id":id,"expected_revision":revision
                }),
            )
            .await
        });
    }
    let (duplicate_id, duplicate_revision) = &publish_requests[0];
    for _ in 0..2 {
        let client = client.clone();
        let url = app.url("");
        let id = duplicate_id.clone();
        let revision = *duplicate_revision;
        tasks.spawn(async move {
            mcp::call(
                &client,
                &url,
                "test-secret",
                "publish_draft",
                json!({
                    "id":id,"expected_revision":revision
                }),
            )
            .await
        });
    }
    let mut results = Vec::new();
    while let Some(result) = tasks.join_next().await {
        results.push(result.unwrap());
    }
    assert_eq!(
        results
            .iter()
            .filter(|result| result["isError"] != true)
            .count(),
        RECIPE_COUNT
    );
    for result in results.iter().filter(|result| result["isError"] == true) {
        assert_eq!(error(result)["code"], "conflict");
    }
    assert_eq!(
        results
            .iter()
            .filter(|result| result["isError"] == true)
            .count(),
        2
    );
}

#[tokio::test]
async fn simultaneous_edit_draft_starts_over_pool_capacity_do_not_exhaust_connections() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let recipe_id = created["recipe"]["id"].as_str().unwrap().to_owned();
    let base_revision = created["recipe"]["revision"].as_str().unwrap().to_owned();
    let client = reqwest::Client::new();
    let mut tasks = tokio::task::JoinSet::new();
    // SQLx's default SQLite pool is 10 connections. Each request starts from the
    // same published revision but must persist an independent owner-private draft.
    for index in 0..12 {
        let client = client.clone();
        let url = app.url("");
        let recipe_id = recipe_id.clone();
        tasks.spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                mcp::call(
                    &client,
                    &url,
                    "test-secret",
                    "save_draft",
                    json!({
                        "recipe_id":recipe_id,
                        "document":{"title":format!("Concurrent edit draft {index}")}
                    }),
                ),
            )
            .await
            .expect("draft start must not wait for a nested pool connection")
        });
    }
    let mut draft_ids = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let response = result.unwrap();
        assert_ne!(response["isError"], true, "{response}");
        let draft: Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(draft["base_recipe_id"], recipe_id);
        assert_eq!(draft["base_revision"], base_revision);
        draft_ids.push(draft["id"].as_str().unwrap().to_owned());
    }
    draft_ids.sort();
    draft_ids.dedup();
    assert_eq!(draft_ids.len(), 12);
    let listed = tool(&app, "list_drafts", json!({})).await;
    assert_eq!(listed.as_array().unwrap().len(), 12);
}

#[tokio::test]
async fn public_pages_scaled_view_and_validation_are_read_only() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let recipe = &created["recipe"];
    let id = recipe["id"].as_str().unwrap();
    let before = app.revision_count().await;
    let ingredient_count = app.ingredient_count().await;

    let index = app.get("/").await;
    assert_eq!(index.status(), 200);
    let index = index.text().await.unwrap();
    assert!(index.contains("<title>Recipes</title>"));
    assert!(!index.contains("wordmark"));
    assert!(index.contains("Butter noodles"));
    assert!(index.contains("No picture"));
    assert!(index.contains("<div class=\"card-image\"><div class=\"placeholder\""));
    assert!(index.contains("<div class=\"card-copy\"><h2 class=\"card-title\">"));
    assert!(!index.contains("card-kicker"));
    assert!(!index.contains("Hearth &amp; Table"));
    assert!(index.contains("href=\"/assets/site.css\""));
    assert!(!index.contains("<style"));
    assert!(index.contains("href=\"/ingredients\""));
    let css = app.get("/assets/site.css").await;
    assert_eq!(css.status(), 200);
    assert!(css.headers()[reqwest::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/css"));
    let css = css.text().await.unwrap();
    assert!(css.contains(":focus-visible"));
    assert!(css.contains(".recipe-card:focus-within"));
    assert!(css.contains(".recipe-card .tag:focus-visible"));
    assert!(css.contains("outline: 3px solid #9a4f26"));
    assert!(css.contains("overflow: hidden"));
    assert!(css.contains(
        ".recipe-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr));"
    ));
    assert!(css.contains(".recipe-grid { grid-template-columns: 1fr;"));
    assert!(css.contains("--color-paper"));
    let font = app.get("/assets/Lora.ttf").await;
    assert_eq!(font.status(), 200);
    assert!(font.headers()[reqwest::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("font/ttf"));
    let detail = app.get(&format!("/recipes/{id}?servings=4")).await;
    assert_eq!(detail.status(), 200);
    let detail = detail.text().await.unwrap();
    assert!(detail.contains("name=\"viewport\""));
    assert!(detail.contains("<title>Butter noodles</title>"));
    assert_eq!(detail.matches(">Recipes</a>").count(), 1);
    assert!(!detail.contains("<script"));
    assert!(detail.contains(">Printable PDF</a>"));
    assert!(detail.contains("value=\"4\""));
    assert!(detail.contains("400 g"));
    assert!(detail.contains("action=\"/recipes/"));
    assert!(detail.contains("id=\"servings\""));
    assert_eq!(app.get("/tags/quick").await.status(), 200);
    let scaled = scaled_ingredients(&app, recipe, 4).await;
    assert_eq!(scaled["ingredients"][1]["quantity"], "400 g");

    let dry_run = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "validate_recipe",
        json!({
            "title": "Preview",
            "servings": 2,
            "target_servings": 4,
            "body": "@butter{20%g}\n@pasta{200%g}\n@unregistered"
        }),
    )
    .await;
    assert!(
        dry_run["resolutions"].as_array().unwrap().last().unwrap()["would_create"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(dry_run["print_fit"]["status"], "fits");
    assert_eq!(dry_run["print_fit"]["font_size_pt"], 11);
    let after = app.revision_count().await;
    assert_eq!(before, after);
    assert_eq!(ingredient_count, app.ingredient_count().await);
    let public_index = app.get("/").await.text().await.unwrap();
    assert!(!public_index.contains("revision"));
}

#[tokio::test]
async fn ingredient_prologue_does_not_duplicate_rows_or_render_as_a_method_step() {
    let app = support::TestApp::start().await;
    let body = "@olive oil{15%ml}\n@onion{1}, diced\n@carrot{2}, sliced\n@celery stalk{2}, sliced\n@garlic clove{2}, minced\n@chicken thigh{600%g}, bone-in\n@chicken stock{1.5%l}\n@bay leaf{1}\n@thyme{2%sprig}\n@salt{1%tsp}\n@black pepper{0.5%tsp}\n@egg noodle{150%g}\n@parsley{10%g}, chopped\n\nHeat @olive oil{} in a large pot over medium heat. Add @onion{}, @carrot{}, and @celery stalk{}; cook until softened. Stir in @garlic clove{} briefly.\n\nAdd @chicken thigh{}, @chicken stock{}, @bay leaf{}, @thyme{}, @salt{}, and @black pepper{}. Bring to a boil, then reduce to a gentle simmer. Cover and cook until the chicken is tender and reaches 74°C at its thickest point.\n\nRemove the chicken. Add @egg noodle{} to the pot and cook until tender according to the package directions. Discard the chicken skin and bones, shred the meat, and return it to the pot. Remove the bay leaf and thyme stems, stir in @parsley{}, and adjust seasoning.";
    let created = publish(
        &app,
        json!({
            "title": "Chicken Soup",
            "description": "A comforting chicken soup with vegetables, noodles, and fresh parsley.",
            "servings": 4,
            "tags": ["soup", "chicken", "comfort food"],
            "body": body
        }),
    )
    .await;
    let id = created["recipe"]["id"].as_str().unwrap();
    let scaled = scaled_ingredients(&app, &created["recipe"], 8).await;
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(
        ingredients.len(),
        13,
        "prologue quantities must not be duplicated"
    );
    assert_eq!(ingredients[0]["name"], "olive oil");
    assert_eq!(ingredients[0]["quantity"], "30 ml");
    assert_eq!(ingredients[1]["note"], "diced");
    assert_eq!(ingredients[5]["name"], "chicken thigh");
    assert_eq!(ingredients[5]["quantity"], "1 2/10 kg");
    assert_eq!(ingredients[5]["note"], "bone-in");
    let html = app
        .get(&format!("/recipes/{id}?servings=8"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("Heat <span class=\"ingredient-name\">olive oil</span> in a large pot"));
    assert_eq!(html.matches("<li id=\"step-").count(), 3);
    assert!(html.contains("<span class=\"gather-note\">diced</span>"));
    assert!(html.contains("Used in <a href=\"#step-1\" aria-label=\"Step 1\">Step 1</a>"));
    assert!(!html.contains("olive oil onion, diced carrot, sliced"));

    let pdf = app
        .get(&format!("/recipes/{id}.pdf?servings=8"))
        .await
        .bytes()
        .await
        .unwrap();
    let document = lopdf::Document::load_mem(&pdf).unwrap();
    assert_eq!(document.get_pages().len(), 1);
    let pdf_text = pdf_extract::extract_text_from_mem(&pdf)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(pdf_text.contains("Heat olive oil in a large pot"));
    assert!(pdf_text.contains("diced"));
    assert!(!pdf_text.contains("olive oil onion, diced carrot, sliced"));
}

#[tokio::test]
async fn method_highlights_cooklang_ingredients_but_not_plain_words_or_cookware() {
    let app = support::TestApp::start().await;
    let created = publish(
        &app,
        json!({
            "title": "Seasoning",
            "servings": 2,
            "body": "Stir @salt{2%g} into the #pan{}, then taste the salt & adjust."
        }),
    )
    .await;
    let id = created["recipe"]["id"].as_str().unwrap();
    let html = app
        .get(&format!("/recipes/{id}?servings=4"))
        .await
        .text()
        .await
        .unwrap();
    let salt = "<span class=\"ingredient-name\">4 g salt</span>";
    assert!(html.contains(
        "<span class=\"gather-name\">salt</span> <span class=\"gather-amount\">4 g</span>"
    ));
    assert!(html.contains(&format!(
        "<p>Stir {salt} into the pan, then taste the salt &amp; adjust.</p>"
    )));
    assert_eq!(html.matches(salt).count(), 1);
    assert!(!html.contains("Used in"));
}

#[tokio::test]
async fn cooking_view_shows_scaled_allocations_and_links_to_named_steps() {
    let app = support::TestApp::start().await;
    let body = "== Toast the crumbs ==\nMelt @butter{20%g}. Add @breadcrumbs{60%g} and toast until crisp.\n\n== Empty the pan ==\nTransfer @&(=1)toasted crumbs{} onto a plate.\n\n== Finish with lemon ==\nMelt @&butter{10%g}. Stir in @lemon zest{2%g}, then toss with the toasted crumbs.";
    let created = publish(
        &app,
        json!({"title": "Lemon-butter crumbs", "servings": 2, "body": body}),
    )
    .await;
    let id = created["recipe"]["id"].as_str().unwrap();
    let html = app
        .get(&format!("/recipes/{id}?servings=4"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains(">Serves 4</p>"));
    assert!(html.contains(">butter</span> <span class=\"gather-amount\">60 g</span>"));
    assert_eq!(html.matches("class=\"gather-name\">butter").count(), 1);
    assert!(html.contains(">40 g butter</span>"));
    assert!(html.contains(">20 g butter</span>"));
    assert!(html.contains("href=\"#step-1\" aria-label=\"Step 1: Toast the crumbs\""));
    assert!(html.contains("href=\"#step-3\" aria-label=\"Step 3: Finish with lemon\""));
    assert!(!html.contains("href=\"#step-2\""));
    assert!(html.contains("id=\"step-2\" tabindex=\"-1\""));
    assert!(html.contains("Transfer toasted crumbs onto a plate."));
    assert!(!html.contains("class=\"gather-name\">toasted crumbs"));
    assert!(html.contains("<a href=\"#method\">Method</a>"));
}

#[tokio::test]
async fn ingredient_only_recipes_and_distinct_measured_occurrences_are_preserved() {
    let app = support::TestApp::start().await;
    let ingredient_only = publish(
        &app,
        json!({"title":"Ingredient-only sauce","servings":2,
        "body":"@butter{20%g}\n@flour{100%g}\n@milk{200%ml}"}),
    )
    .await;
    let id = ingredient_only["recipe"]["id"].as_str().unwrap();
    let scaled = scaled_ingredients(&app, &ingredient_only["recipe"], 2).await;
    assert_eq!(scaled["ingredients"].as_array().unwrap().len(), 3);
    let html = app
        .get(&format!("/recipes/{id}"))
        .await
        .text()
        .await
        .unwrap();
    assert!(!html.contains("<li id=\"step-"));

    let unspecified = publish(
        &app,
        json!({"title":"Unquantified ingredients","servings":2,"body":"@salt\n@pepper"}),
    )
    .await;
    let id = unspecified["recipe"]["id"].as_str().unwrap();
    let scaled = scaled_ingredients(&app, &unspecified["recipe"], 2).await;
    assert_eq!(scaled["ingredients"].as_array().unwrap().len(), 2);
    let html = app
        .get(&format!("/recipes/{id}"))
        .await
        .text()
        .await
        .unwrap();
    assert!(!html.contains("<li id=\"step-"));

    let measured = publish(&app, json!({"title":"Measured sauce","servings":2,
        "body":"For the base use @butter{20%g} and @butter{10%g}. Melt @butter{} in #saucepan{} for ~{1%minute}, then add @flour{15%g} and whisk in @milk{200%ml}."})).await;
    let id = measured["recipe"]["id"].as_str().unwrap();
    let scaled = scaled_ingredients(&app, &measured["recipe"], 2).await;
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(ingredients.len(), 5);
    assert_eq!(
        ingredients
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["butter", "butter", "butter", "flour", "milk"]
    );
    assert_eq!(
        ingredients
            .iter()
            .map(|item| item["quantity"].as_str())
            .collect::<Vec<_>>(),
        [
            Some("20 g"),
            Some("10 g"),
            None,
            Some("15 g"),
            Some("200 ml")
        ]
    );
    let html = app
        .get(&format!("/recipes/{id}"))
        .await
        .text()
        .await
        .unwrap();
    assert_eq!(html.matches("<li id=\"step-").count(), 1);
    assert!(html.contains("saucepan"));
    assert!(html.contains("1 minute"));
}

#[tokio::test]
async fn declaration_detection_is_structural_and_only_suppresses_block_references() {
    let app = support::TestApp::start().await;
    let instruction = publish(
        &app,
        json!({"title":"Fried onion","servings":1,
        "body":"@onion{1}, fry until golden\n\nServe the onion."}),
    )
    .await;
    let id = instruction["recipe"]["id"].as_str().unwrap();
    let scaled = scaled_ingredients(&app, &instruction["recipe"], 1).await;
    assert_eq!(scaled["ingredients"].as_array().unwrap().len(), 1);
    let html = app
        .get(&format!("/recipes/{id}"))
        .await
        .text()
        .await
        .unwrap();
    assert_eq!(html.matches("<li id=\"step-").count(), 2);
    assert!(html.contains("fry until golden"));
    assert!(html.contains("Serve the onion"));

    let repeated = publish(
        &app,
        json!({"title":"Repeated onions","servings":1,
        "body":"Add @onion{}(diced) to the pan. Stir in @onion{}(thinly sliced)."}),
    )
    .await;
    let scaled = scaled_ingredients(&app, &repeated["recipe"], 1).await;
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(
        ingredients.len(),
        2,
        "ordinary repeated references are not deduplicated"
    );
    assert_eq!(ingredients[0]["note"], "diced");
    assert_eq!(ingredients[1]["note"], "thinly sliced");

    let declared_then_prepped = publish(
        &app,
        json!({"title":"Onion occurrences","servings":1,
        "body":"@onion{1}\n@salt{1%pinch}\n\nAdd @onion{}(thinly sliced) to the pan."}),
    )
    .await;
    let scaled = scaled_ingredients(&app, &declared_then_prepped["recipe"], 1).await;
    let ingredients = scaled["ingredients"].as_array().unwrap();
    assert_eq!(
        ingredients.len(),
        3,
        "a meaningful later occurrence note keeps its own row"
    );
    assert_eq!(ingredients[2]["name"], "onion");
    assert_eq!(ingredients[2]["note"], "thinly sliced");
}

#[tokio::test]
async fn public_browsing_filters_and_canonical_ingredient_directory_work_over_http() {
    let app = support::TestApp::start().await;
    let soup = publish(
        &app,
        json!({"title":"Chicken Soup","servings":2,"tags":["soup"],
        "body":"@chicken{200%g}\n@chicken{50%g}\n@onion{1%piece}"}),
    )
    .await;
    let second = publish(&app, json!({"title":"Chicken Stew","servings":2,"tags":["soup","dinner"],"body":"@chicken{300%g}"})).await;
    let unrelated = publish(
        &app,
        json!({"title":"Toast","servings":2,"tags":["quick"],"body":"@bread{2%slice}"}),
    )
    .await;
    publish(&app, json!({"title":"<script>alert(1)</script>","servings":2,"tags":["soup"],"body":"@pepper\n@chicken"})).await;
    publish(
        &app,
        json!({"title":"Chicken Variant","servings":2,"tags":["dinner"],"body":"@chickens{200%g}"}),
    )
    .await;
    let soup_id = soup["recipe"]["id"].as_str().unwrap();
    let second_id = second["recipe"]["id"].as_str().unwrap();
    let unrelated_id = unrelated["recipe"]["id"].as_str().unwrap();

    let listing = app.get("/recipes?tag=soup").await;
    assert_eq!(
        listing.status(),
        200,
        "/recipes must be a public listing route"
    );
    let listing = listing.text().await.unwrap();
    assert!(listing.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(listing.contains("<h2 class=\"card-title\">Chicken Stew</h2>"));
    assert!(listing.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!listing.contains("<script>alert(1)</script>"));
    assert!(!listing.contains("<h2 class=\"card-title\">Toast</h2>"));
    assert!(listing.contains("href=\"/recipes/"));
    assert!(app.get("/tags/soup").await.status().is_success());
    let matching = app
        .get("/recipes?q=Chicken&tag=soup")
        .await
        .text()
        .await
        .unwrap();
    assert!(matching.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(matching.contains("<h2 class=\"card-title\">Chicken Stew</h2>"));
    assert!(!matching.contains("<h2 class=\"card-title\">Chicken Variant</h2>"));
    assert_eq!(matching.matches("Clear filters").count(), 1);
    let wrong_tag = app
        .get("/recipes?q=Chicken&tag=quick")
        .await
        .text()
        .await
        .unwrap();
    assert!(wrong_tag.contains("No recipes"));
    assert!(!wrong_tag.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));

    let search = app.get("/?q=chicken&tag=").await;
    assert_eq!(search.status(), 200);
    let search = search.text().await.unwrap();
    assert!(search.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(search.contains("<h2 class=\"card-title\">Chicken Stew</h2>"));
    assert!(search.contains("href=\"/recipes?tag=soup\">soup</a>"));
    assert!(search.contains("href=\"/ingredients\""));
    assert_eq!(search.matches("Clear filters").count(), 1);

    let whitespace_search = app
        .get("/recipes?q=%20Chicken%20&tag=%20soup%20")
        .await
        .text()
        .await
        .unwrap();
    assert!(whitespace_search.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(whitespace_search.contains("value=\" Chicken \""));
    assert!(whitespace_search.contains("value=\" soup \""));
    assert_eq!(whitespace_search.matches("Clear filters").count(), 1);
    let blank_search = app
        .get("/recipes?q=%20%20&tag=%20")
        .await
        .text()
        .await
        .unwrap();
    assert!(blank_search.contains("<h2 class=\"card-title\">Chicken Soup</h2>"));
    assert!(!blank_search.contains("Clear filters"));
    let escaped_filter = app
        .get("/recipes?q=%3Cscript%3E")
        .await
        .text()
        .await
        .unwrap();
    assert!(escaped_filter.contains("value=\"&lt;script&gt;\""));
    assert!(!escaped_filter.contains("value=\"<script>\""));
    assert_eq!(escaped_filter.matches("Clear filters").count(), 1);

    let no_match = app
        .get("/recipes?q=does-not-exist&tag=soup")
        .await
        .text()
        .await
        .unwrap();
    assert!(no_match.contains("No recipes"));
    assert!(no_match.contains("Clear filters"));
    assert_eq!(no_match.matches("Clear filters").count(), 1);

    let directory = app.get("/ingredients").await;
    assert_eq!(directory.status(), 200);
    let directory = directory.text().await.unwrap();
    assert!(directory.contains("<title>Ingredients</title>"));
    assert!(directory.contains("<h1 class=\"collection-title\">Ingredients</h1>"));
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(directory.contains("<h2>chicken</h2>"));
    let headings = directory
        .split("<h2>")
        .skip(1)
        .map(|section| section.split("</h2>").next().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        headings,
        ["bread", "chicken", "chickens", "onion", "pepper"]
    );
    assert!(chicken.contains("3 recipes"));
    assert!(chicken.contains(&format!("/recipes/{soup_id}")));
    assert!(chicken.contains(&format!("/recipes/{second_id}")));
    assert!(!chicken.contains(&format!("/recipes/{unrelated_id}")));
    assert!(directory.contains("<h2>onion</h2>"));
    let linked_id = chicken
        .split("href=\"/recipes/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let linked_recipe = app.get(&format!("/recipes/{linked_id}")).await;
    assert_eq!(linked_recipe.status(), 200);
    let linked_recipe = linked_recipe.text().await.unwrap();
    assert!(linked_recipe.contains("<h1>"));

    let client = app.client();
    let url = app.url("");
    let ingredients = mcp::tool(&client, &url, "test-secret", "list_ingredients", json!({})).await;
    let ingredient_id = |name: &str| {
        ingredients
            .as_array()
            .unwrap()
            .iter()
            .find(|ingredient| ingredient["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let chicken_id = ingredient_id("chicken");
    let chickens_id = ingredient_id("chickens");
    let merged = mcp::call(
        &client,
        &url,
        "test-secret",
        "merge_ingredients",
        json!({"id":chickens_id,"survivor":chicken_id}),
    )
    .await;
    assert_ne!(merged["isError"], true);
    let directory = app.get("/ingredients").await.text().await.unwrap();
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(chicken.contains("4 recipes"));
    assert!(!directory.contains("<h2>chickens</h2>"));
    assert!(directory.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!directory.contains("<script>alert(1)</script>"));

    let pepper_id = ingredient_id("pepper");
    let renamed = mcp::call(
        &client,
        &url,
        "test-secret",
        "rename_ingredient",
        json!({"id":pepper_id,"name":"pepper <b>hot</b>"}),
    )
    .await;
    assert_ne!(renamed["isError"], true);
    let directory = app.get("/ingredients").await.text().await.unwrap();
    assert!(directory.contains("pepper &lt;b&gt;hot&lt;/b&gt;"));
    assert!(!directory.contains("pepper <b>hot</b>"));

    let edit = mcp::tool(
        &client,
        &url,
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":soup_id,"document":{"body":"@rice{200%g}"}
        }),
    )
    .await;
    mcp::tool(
        &client,
        &url,
        "test-secret",
        "publish_draft",
        json!({
            "id":edit["id"],"expected_revision":edit["revision"]
        }),
    )
    .await;
    let directory = app.get("/ingredients").await.text().await.unwrap();
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(chicken.contains("3 recipes"));
    assert!(!chicken.contains(&format!("/recipes/{soup_id}")));

    let rev = second["recipe"]["revision"].as_str().unwrap();
    let deleted = mcp::tool(
        &client,
        &url,
        "test-secret",
        "delete_recipe",
        json!({
            "id":second_id,"base_revision":rev
        }),
    )
    .await;
    assert_eq!(deleted["deleted"], true);
    let directory = app.get("/ingredients").await.text().await.unwrap();
    let chicken = directory
        .split("<h2>chicken</h2>")
        .nth(1)
        .unwrap()
        .split("</article>")
        .next()
        .unwrap();
    assert!(chicken.contains("2 recipes"));
    assert!(!chicken.contains(&format!("/recipes/{second_id}")));
}

#[tokio::test]
async fn public_pdf_is_a_deterministic_single_page_a4_render() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let id = created["recipe"]["id"].as_str().unwrap();

    let public = app.get(&format!("/recipes/{id}.pdf?servings=4")).await;
    assert_eq!(public.status(), 200);
    assert_eq!(
        public.headers().get("content-type").unwrap(),
        "application/pdf"
    );
    let public = public.bytes().await.unwrap();
    assert!(public.starts_with(b"%PDF-"));
    let document = lopdf::Document::load_mem(&public).unwrap();
    let pages = document.get_pages();
    assert_eq!(pages.len(), 1, "PDF must contain exactly one page");
    let page = document
        .get_object(*pages.values().next().unwrap())
        .unwrap()
        .as_dict()
        .unwrap();
    let media_box = page.get(b"MediaBox").unwrap().as_array().unwrap();
    let width = media_box[2].as_float().unwrap();
    let height = media_box[3].as_float().unwrap();
    assert!((width - 595.276).abs() < 0.01);
    assert!((height - 841.89).abs() < 0.01);

    let repeat = app.get(&format!("/recipes/{id}.pdf?servings=4")).await;
    assert_eq!(repeat.status(), 200);
    assert_eq!(repeat.bytes().await.unwrap(), public);

    let html = app
        .get(&format!("/recipes/{id}?servings=4"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("400 g"));
    assert!(html.contains(&format!("/recipes/{id}.pdf?servings=4")));
    mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "delete_recipe",
        json!({"id":id,"base_revision":created["recipe"]["revision"]}),
    )
    .await;
    assert_eq!(
        app.get(&format!("/recipes/{id}.pdf?servings=4"))
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn pdf_uses_scaled_fractional_and_fixed_quantities_and_fits_long_recipes() {
    let app = support::TestApp::start().await;
    let recipe = publish(
        &app,
        json!({
            "title": "Sauce",
            "servings": 2,
            "body": "@pepper{=1%g}\n@butter{20%g}\n@flour{10%g}\n@sugar{1%g}"
        }),
    )
    .await["recipe"]
        .clone();
    let id = recipe["id"].as_str().unwrap();
    let html = app
        .get(&format!("/recipes/{id}?servings=3"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("1 g"));
    assert!(html.contains("30 g"));
    assert!(html.contains("15 g"));
    assert!(html.contains("1 1/2 g"), "HTML was: {html}");

    let scaled = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "validate_recipe",
        json!({
            "title":recipe["title"],"servings":2,"body":recipe["body"],"target_servings":3
        }),
    )
    .await;
    let quantities = scaled["ingredients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ingredient| ingredient["quantity"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(quantities, ["1 g", "30 g", "15 g", "1 1/2 g"]);

    let pdf = app
        .get(&format!("/recipes/{id}.pdf?servings=3"))
        .await
        .bytes()
        .await
        .unwrap();
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap();
    assert!(text.contains("1 g"), "PDF text was: {text}");
    assert!(text.contains("30 g"), "PDF text was: {text}");
    assert!(text.contains("15 g"), "PDF text was: {text}");
    assert!(text.contains("1 1/2 g"), "PDF text was: {text}");

    let long_body = (0..70)
        .map(|index| format!("Mix for step {index}: add a careful pinch of seasoning."))
        .collect::<Vec<_>>()
        .join("\n");
    let long = publish(
        &app,
        json!({"title":"Long recipe","servings":2,"body":long_body}),
    )
    .await["recipe"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fit = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "validate_recipe",
        json!({
            "title":"Long recipe","servings":2,"body":long_body
        }),
    )
    .await;
    assert_eq!(fit["print_fit"]["fits"], true);
    assert!(fit["print_fit"]["font_size_pt"].as_u64().unwrap() < 11);

    let pdf = app.get(&format!("/recipes/{long}.pdf")).await;
    assert_eq!(pdf.status(), 200);
    let pdf = pdf.bytes().await.unwrap();
    assert_eq!(
        pdf_extract::extract_text_from_mem_by_pages(&pdf)
            .unwrap()
            .len(),
        1
    );
    let accepted_text = pdf_extract::extract_text_from_mem(&pdf).unwrap();
    assert!(
        accepted_text.contains("step 69"),
        "PDF text was: {accepted_text}"
    );
}

#[tokio::test]
async fn shared_outputs_preserve_timers_tiny_quantities_ranges_notes_tags_and_source() {
    let app = support::TestApp::start().await;
    let created = publish(&app, json!({
            "title": "Details",
            "description": "Candidate description",
            "servings": 2,
            "tags": ["baking/sweet?yes#later"],
            "notes": "Rest before serving.",
            "source_url": "https://example.com/recipe",
            "body": "Add @salt{0.000001%g}(fine-grained), @pepper{1-2%g}, and @trace{0.0004-0.0008%g}.\nLet the dough rest for ~{1%hour}.\nAllow ~rest{2%minutes} before serving."
        })).await["recipe"].clone();
    let id = created["id"].as_str().unwrap();
    let html = app
        .get(&format!("/recipes/{id}"))
        .await
        .text()
        .await
        .unwrap();
    assert!(html.contains("fine-grained"));
    assert!(html.contains("0.000001 g"), "{html}");
    assert!(
        html.contains("1-2 g") || html.contains("1 to 2 g"),
        "{html}"
    );
    assert!(html.contains("0.0004-0.0008 g"), "{html}");
    assert!(html.contains("1 hour"), "{html}");
    assert!(html.contains("rest (2 minutes)"), "{html}");
    assert!(html.contains("Rest before serving."));
    assert!(html.contains("https://example.com/recipe"));
    assert!(html.contains("/recipes?tag=baking%2Fsweet%3Fyes%23later"));
    assert!(!html.contains("/tags/baking/"));

    let pdf = app
        .get(&format!("/recipes/{id}.pdf"))
        .await
        .bytes()
        .await
        .unwrap();
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("1 hour") || text.contains("1hour"), "{text}");
    for expected in [
        "0.000001 g",
        "1-2 g",
        "0.0004-0.0008 g",
        "rest",
        "2 minutes",
        "fine-grained",
        "Rest before serving.",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }

    let candidate = json!({
        "title": "Candidate",
        "description": "Full candidate description",
        "servings": 2,
        "body": "@rice{100%g}\nLet it rest for ~{5%minutes}.",
        "notes": "Candidate note.",
        "source_url": "https://example.org/source",
        "tags": ["rice"],
        "target_servings": 3
    });
    let fit = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "validate_recipe",
        candidate.clone(),
    )
    .await;
    assert_eq!(fit["print_fit"]["fits"], true);
    let mut document = candidate.clone();
    document.as_object_mut().unwrap().remove("target_servings");
    let actual = publish(&app, document).await;
    let actual_id = actual["recipe"]["id"].as_str().unwrap();
    let actual_pdf = app
        .get(&format!("/recipes/{actual_id}.pdf?servings=3"))
        .await
        .bytes()
        .await
        .unwrap();
    let pdf_text = pdf_extract::extract_text_from_mem(&actual_pdf).unwrap();
    assert!(pdf_text.contains("Full candidate description"));
    assert!(pdf_text.contains("Candidate note."));
    assert!(pdf_text.contains("5 minutes"));
}

#[tokio::test]
async fn oversized_recipe_returns_structured_422_without_a_partial_pdf() {
    let app = support::TestApp::start().await;
    let body = (0..300)
        .map(|index| format!("Step {index}: {}", "carefully stir and cook ".repeat(14)))
        .collect::<Vec<_>>()
        .join("\n");
    let id = publish(
        &app,
        json!({"title":"Impossible recipe","servings":2,"body":body.clone()}),
    )
    .await["recipe"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = app.get(&format!("/recipes/{id}.pdf")).await;
    assert_eq!(response.status(), 422);
    let error: Value = response.json().await.unwrap();
    assert_eq!(error["code"], "print_does_not_fit");
    assert_eq!(error["details"]["minimum_font_size_pt"], 8);
    assert!(
        error["details"]["page_count_at_minimum_font_size"]
            .as_u64()
            .unwrap()
            > 1
    );

    let validation = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "validate_recipe",
        json!({
            "title":"Impossible recipe","servings":2,"body":body
        }),
    )
    .await;
    assert_eq!(validation["print_fit"]["fits"], false);
    assert_eq!(validation["print_fit"]["font_size_pt"], Value::Null);
    assert_eq!(validation["print_fit"]["status"], "does_not_fit");
}

#[tokio::test]
async fn pdf_embeds_attached_picture() {
    let app = support::TestApp::start().await;
    let recipe = create(&app).await;
    let recipe = &recipe["recipe"];
    let id = recipe["id"].as_str().unwrap();
    let mut jpeg = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(24, 24, image::Rgb([220, 30, 40]))
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    app.install_image_fixture("img_pdf_fixture", jpeg.get_ref());
    mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "attach_image",
        json!({
            "id":id,"base_revision":recipe["revision"],"image_ref":"img_pdf_fixture"
        }),
    )
    .await;

    let fit = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "validate_recipe",
        json!({
            "title": recipe["title"],
            "description": recipe["description"],
            "servings": recipe["servings"],
            "tags": ["quick"],
            "body": "@butter{20%g}\n@pasta{200%g}",
            "image_ref": "img_pdf_fixture"
        }),
    )
    .await;
    assert_eq!(fit["print_fit"]["fits"], true);

    let pdf = app
        .get(&format!("/recipes/{id}.pdf"))
        .await
        .bytes()
        .await
        .unwrap();
    assert!(
        pdf.windows(b"/Subtype /Image".len())
            .any(|window| window == b"/Subtype /Image"),
        "attached picture must be embedded in the PDF"
    );
}

#[tokio::test]
async fn authenticated_crud_keeps_immutable_history_and_rejects_stale_writes() {
    let app = support::TestApp::start().await;
    let (stored_hash, label) = app.stored_token().await;
    assert_eq!(label, "integration");
    assert_eq!(stored_hash.len(), 64);
    assert_ne!(stored_hash, "test-secret");
    let unauthenticated_write = reqwest::Client::new()
        .post(app.url("/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"save_draft","arguments":{"document":{}}}}))
        .send()
        .await
        .expect("unauthenticated request");
    assert_eq!(unauthenticated_write.status(), 401);

    let first = create(&app).await;
    let id = first["recipe"]["id"].as_str().unwrap();
    let rev1 = first["recipe"]["revision"].as_str().unwrap();
    assert_eq!(first["recipe"]["author_label"], "integration");
    assert!(first["recipe"]["revision_created_at"]
        .as_str()
        .unwrap()
        .contains('T'));

    let update = json!({
        "title": "Butter noodles",
        "description": "Updated",
        "servings": 3,
        "tags": ["quick", "edited"],
        "body": "@butter{30%g}\n@pasta{300%g}"
    });
    let stale_draft = tool(
        &app,
        "save_draft",
        json!({"recipe_id":id,"document":update}),
    )
    .await;
    let first_draft = tool(
        &app,
        "save_draft",
        json!({"recipe_id":id,"document":update}),
    )
    .await;
    let second = tool(
        &app,
        "publish_draft",
        json!({"id":first_draft["id"],"expected_revision":first_draft["revision"]}),
    )
    .await;
    let rev2 = second["recipe"]["revision"].as_str().unwrap();

    let second_update = json!({
        "title": "Butter noodles",
        "description": "Edited a second time",
        "servings": 4,
        "tags": ["quick", "edited"],
        "body": "@butter{40%g}\n@pasta{400%g}"
    });
    let edit = tool(
        &app,
        "save_draft",
        json!({"recipe_id":id,"document":second_update}),
    )
    .await;
    assert_eq!(edit["base_revision"], rev2);
    let third = tool(
        &app,
        "publish_draft",
        json!({"id":edit["id"],"expected_revision":edit["revision"]}),
    )
    .await;
    let rev3 = third["recipe"]["revision"].as_str().unwrap();

    let stale = call(
        &app,
        "test-secret",
        "publish_draft",
        json!({"id":stale_draft["id"],"expected_revision":stale_draft["revision"]}),
    )
    .await;
    let stale = error(&stale);
    assert_eq!(stale["code"], "conflict");
    assert_eq!(stale["current_revision_id"], rev3);

    let history = tool(&app, "list_revisions", json!({"id":id})).await;
    assert_eq!(history.as_array().unwrap().len(), 3);
    let historical = tool(&app, "get_revision", json!({"id":id,"revision":rev1})).await;
    assert_eq!(historical["description"], "A simple supper");

    let deleted = tool(&app, "delete_recipe", json!({"id":id,"base_revision":rev3})).await;
    assert_eq!(deleted["deleted"], true);
    assert_eq!(app.get(&format!("/recipes/{id}.pdf")).await.status(), 404);
    let list = tool(&app, "list_recipes", json!({})).await;
    assert!(list.as_array().unwrap().is_empty());

    let restored = tool(
        &app,
        "restore_recipe",
        json!({"id":id,"base_revision":deleted["revision"]}),
    )
    .await;
    assert_eq!(restored["deleted"], false);
    assert_ne!(restored["revision"], rev3);
    let history = tool(&app, "list_revisions", json!({"id":id})).await;
    assert_eq!(history.as_array().unwrap().len(), 5);

    let restored_old = tool(
        &app,
        "restore_revision",
        json!({"id":id,"revision":rev1,"base_revision":restored["revision"]}),
    )
    .await;
    assert_eq!(restored_old["description"], "A simple supper");
    assert_ne!(restored_old["revision"], rev1);
}

#[tokio::test]
async fn only_one_concurrent_update_from_the_same_base_can_win() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let id = created["recipe"]["id"].as_str().unwrap();
    let client = reqwest::Client::new();
    let base_url = app.url("");
    let left = mcp::tool(
        &client,
        &base_url,
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":id,"document":{"title":"Left edit","servings":2,"body":"@rice{100%g}"}
        }),
    )
    .await;
    let right = mcp::tool(
        &client,
        &base_url,
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":id,"document":{"title":"Right edit","servings":3,"body":"@rice{150%g}"}
        }),
    )
    .await;
    let (left, right) = tokio::join!(
        mcp::call(
            &client,
            &base_url,
            "test-secret",
            "publish_draft",
            json!({
                "id":left["id"],"expected_revision":left["revision"]
            })
        ),
        mcp::call(
            &client,
            &base_url,
            "test-secret",
            "publish_draft",
            json!({
                "id":right["id"],"expected_revision":right["revision"]
            })
        )
    );
    assert_ne!(left["isError"], right["isError"]);
    let conflict = if left["isError"] == true { left } else { right };
    let error: Value =
        serde_json::from_str(conflict["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], "conflict");
    let history = mcp::tool(
        &client,
        &base_url,
        "test-secret",
        "list_revisions",
        json!({"id":id}),
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn stale_revision_wins_over_invalid_cooklang_and_invalid_current_update_rolls_back() {
    let app = support::TestApp::start().await;
    let created = create(&app).await;
    let id = created["recipe"]["id"].as_str().unwrap();
    let client = app.client();
    let url = app.url("");
    let stale_draft = mcp::tool(
        &client,
        &url,
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":id,"document":{"title":"Invalid stale edit","body":"~rest{}"}
        }),
    )
    .await;
    let first_draft = mcp::tool(
        &client,
        &url,
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":id,"document":{"title":"Committed","body":"@rice{100%g}"}
        }),
    )
    .await;
    let first_update = mcp::tool(
        &client,
        &url,
        "test-secret",
        "publish_draft",
        json!({
            "id":first_draft["id"],"expected_revision":first_draft["revision"]
        }),
    )
    .await;
    let current_revision = first_update["recipe"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();

    let stale_invalid = mcp::call(
        &client,
        &url,
        "test-secret",
        "publish_draft",
        json!({
            "id":stale_draft["id"],"expected_revision":stale_draft["revision"]
        }),
    )
    .await;
    assert_eq!(stale_invalid["isError"], true);
    let error: Value =
        serde_json::from_str(stale_invalid["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], "conflict");
    assert_eq!(error["current_revision_id"], current_revision);

    let invalid_draft = mcp::tool(
        &client,
        &url,
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":id,"document":{"title":"Invalid current edit","body":"~rest{}"}
        }),
    )
    .await;
    let invalid_current = mcp::call(
        &client,
        &url,
        "test-secret",
        "publish_draft",
        json!({
            "id":invalid_draft["id"],"expected_revision":invalid_draft["revision"]
        }),
    )
    .await;
    assert_eq!(invalid_current["isError"], true);
    let error: Value =
        serde_json::from_str(invalid_current["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], "cooklang_invalid");
    let current = mcp::tool(&client, &url, "test-secret", "get_recipe", json!({"id":id})).await;
    assert_eq!(current["revision"], current_revision);
    assert_eq!(current["title"], "Committed");
}

#[tokio::test]
async fn structured_recipe_validation_and_guidelines_are_available() {
    let app = support::TestApp::start().await;
    let client = app.client();
    let url = app.url("");
    let invalid_cooklang = mcp::call(
        &client,
        &url,
        "test-secret",
        "validate_recipe",
        json!({
            "title": "Invalid body",
            "servings": 1,
            "body": "First step\n~rest{}"
        }),
    )
    .await;
    assert_eq!(invalid_cooklang["isError"], true);
    let diagnostic: Value =
        serde_json::from_str(invalid_cooklang["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(diagnostic["code"], "cooklang_invalid");
    assert_eq!(diagnostic["diagnostics"][0]["line"], 2);
    assert!(diagnostic["diagnostics"][0]["column"].as_u64().is_some());
    assert!(!diagnostic["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .is_empty());
    let unchanged = mcp::tool(&client, &url, "test-secret", "list_recipes", json!({})).await;
    assert!(unchanged.as_array().unwrap().is_empty());

    for body in [
        "---\ntitle: duplicated\n---\n@rice{100%g}",
        ">> servings: 1\n@rice{100%g}",
    ] {
        let rejected = mcp::call(
            &client,
            &url,
            "test-secret",
            "validate_recipe",
            json!({
                "title": "Cooklang metadata",
                "servings": 1,
                "body": body
            }),
        )
        .await;
        assert_eq!(rejected["isError"], true);
        let error: Value =
            serde_json::from_str(rejected["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(error["code"], "cooklang_metadata_forbidden");
    }

    let malformed_yaml = mcp::call(
        &client,
        &url,
        "test-secret",
        "validate_recipe",
        json!({
            "title": "Malformed Cooklang metadata",
            "servings": 1,
            "body": "---\ntitle: duplicated\n---\n~rest{}"
        }),
    )
    .await;
    assert_eq!(malformed_yaml["isError"], true);
    let yaml_error: Value =
        serde_json::from_str(malformed_yaml["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(yaml_error["code"], "cooklang_invalid");
    assert!(!yaml_error["diagnostics"].as_array().unwrap().is_empty());

    let incomplete = mcp::call(
        &client,
        &url,
        "test-secret",
        "validate_recipe",
        json!({"title":"Missing required fields"}),
    )
    .await;
    assert_eq!(incomplete["isError"], true);

    let created = create(&app).await;
    assert_eq!(created["warnings"].as_array().unwrap().len(), 0);
    let guideline = mcp::call(&client, &url, "test-secret", "get_guidelines", json!({})).await;
    assert_ne!(guideline["isError"], true);
    let text = guideline["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("temperatures"));
    assert!(text.contains("Never invent"));
}
