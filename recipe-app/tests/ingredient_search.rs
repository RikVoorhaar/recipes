#[path = "support/mcp.rs"]
mod mcp;
#[allow(dead_code)]
mod support;

use serde_json::{json, Value};

async fn create(app: &support::TestApp, name: &str, title: &str) -> Value {
    let client = app.client();
    let base = app.url("");
    let draft = mcp::tool(&client, &base, "test-secret", "save_draft", json!({
        "document":{"title":title,"servings":2,"tags":["supper"],"body":format!("@{name}{{100%g}}")}
    })).await;
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

async fn create_document(
    app: &support::TestApp,
    title: &str,
    description: &str,
    notes: &str,
    servings: u32,
    tags: &[&str],
    body: &str,
) -> Value {
    let client = app.client();
    let base = app.url("");
    let draft = mcp::tool(
        &client,
        &base,
        "test-secret",
        "save_draft",
        json!({
            "document":{"title":title,"description":description,"notes":notes,
                "servings":servings,"tags":tags,"body":body}
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
    .await
}

async fn search(app: &support::TestApp, query: &str) -> Value {
    let url = reqwest::Url::parse(&format!("http://localhost/?{query}")).unwrap();
    let filters: serde_json::Map<String, Value> = url
        .query_pairs()
        .map(|(key, value)| {
            let value = match key.as_ref() {
                "min_servings" | "max_servings" | "limit" | "offset" => {
                    json!(value.parse::<u64>().unwrap())
                }
                "has_image" | "include_deleted" => json!(value.parse::<bool>().unwrap()),
                _ => json!(value.as_ref()),
            };
            (key.into_owned(), value)
        })
        .collect();
    mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "search_recipes",
        json!({"query":filters}),
    )
    .await
}

async fn call(app: &support::TestApp, name: &str, args: Value) -> Value {
    mcp::call(&app.client(), &app.url(""), "test-secret", name, args).await
}

fn assert_tool_error(result: &Value, code: &str) {
    assert_eq!(result["isError"], true);
    let error: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], code);
}

#[tokio::test]
async fn registry_alias_rename_merge_and_rebuild_keep_recipe_history_immutable() {
    let app = support::TestApp::start().await;
    let tomato_recipe = create(&app, "tomato", "Tomato supper").await["recipe"].clone();
    let tomatoes_recipe = create(&app, "tomatoes", "Tomatoes supper").await["recipe"].clone();
    let list = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_ingredients",
        json!({}),
    )
    .await;
    let tomato = list
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "tomato")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tomatoes = list
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "tomatoes")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(list.as_array().unwrap().len(), 2);
    let tomato_detail = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "get_ingredient",
        json!({"id":tomato}),
    )
    .await;
    assert_eq!(tomato_detail["usage_count"], 1);
    assert_eq!(tomato_detail["recipe_ids"], json!([tomato_recipe["id"]]));
    assert_eq!(
        list.as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "tomato")
            .unwrap()["usage_count"],
        1
    );
    let before_merge = search(&app, "ingredients=tomatoes").await;
    assert_eq!(before_merge.as_array().unwrap().len(), 1);
    assert_eq!(before_merge[0]["id"], tomatoes_recipe["id"]);

    let added = call(
        &app,
        "add_ingredient_alias",
        json!({"id":tomato,"alias":"  pomodoro  "}),
    )
    .await;
    assert_ne!(added["isError"], true);
    let alias_collision = call(
        &app,
        "add_ingredient_alias",
        json!({"id":tomatoes,"alias":"pomodoro"}),
    )
    .await;
    assert_tool_error(&alias_collision, "conflict");
    let canonical_collision = call(
        &app,
        "add_ingredient_alias",
        json!({"id":tomato,"alias":"tomatoes"}),
    )
    .await;
    assert_tool_error(&canonical_collision, "conflict");
    assert_eq!(
        search(&app, "ingredients=pomodoro")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let removed = call(
        &app,
        "remove_ingredient_alias",
        json!({"id":tomato,"alias":"pomodoro"}),
    )
    .await;
    assert_ne!(removed["isError"], true);
    assert!(search(&app, "ingredients=pomodoro")
        .await
        .as_array()
        .unwrap()
        .is_empty());

    let renamed = call(
        &app,
        "rename_ingredient",
        json!({"id":tomato,"name":"red tomato"}),
    )
    .await;
    assert_ne!(renamed["isError"], true);
    let self_merge_by_name = call(
        &app,
        "merge_ingredients",
        json!({"id":tomato,"survivor":"RED TOMATO"}),
    )
    .await;
    assert_tool_error(&self_merge_by_name, "invalid_request");
    assert_eq!(
        search(&app, "ingredients=tomato")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search(&app, "ingredients=red%20tomato")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let merged = call(
        &app,
        "merge_ingredients",
        json!({"id":tomatoes,"survivor":tomato}),
    )
    .await;
    assert_ne!(merged["isError"], true);
    assert_eq!(app.ingredient_history_count(&tomato).await, 5);
    assert_eq!(app.ingredient_history_count(&tomatoes).await, 2);
    let removed_merge_alias = call(
        &app,
        "remove_ingredient_alias",
        json!({"id":tomato,"alias":"tomatoes"}),
    )
    .await;
    assert_tool_error(&removed_merge_alias, "not_found");
    assert_eq!(
        search(&app, "ingredients=tomatoes")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let tag_counts = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_tags",
        json!({}),
    )
    .await;
    assert_eq!(tag_counts[0]["count"], 2);

    app.rebuild_derived().await;
    assert_eq!(
        search(&app, "ingredients=tomatoes")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let revision = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_revisions",
        json!({"id":tomato_recipe["id"]}),
    )
    .await;
    assert_eq!(revision.as_array().unwrap().len(), 1);
    assert_eq!(tomatoes_recipe["body"], "@tomatoes{100%g}");
}

#[tokio::test]
async fn used_alias_provenance_survives_removal_and_rebuild_and_redirect_names_resolve() {
    let app = support::TestApp::start().await;
    let garlic = create(&app, "garlic", "Garlic prep").await["recipe"].clone();
    let ingredients = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_ingredients",
        json!({}),
    )
    .await;
    let garlic_id = ingredients
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "garlic")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_ne!(
        call(
            &app,
            "add_ingredient_alias",
            json!({"id":garlic_id,"alias":"allium sativum"})
        )
        .await["isError"],
        true
    );
    let used = create(&app, "allium sativum", "Alias prep").await["recipe"].clone();
    assert_ne!(
        call(
            &app,
            "remove_ingredient_alias",
            json!({"id":garlic_id,"alias":"allium sativum"})
        )
        .await["isError"],
        true
    );
    app.rebuild_derived().await;
    let by_canonical = search(&app, &format!("ingredients={garlic_id}")).await;
    assert_eq!(
        by_canonical
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![used["id"].as_str().unwrap(), garlic["id"].as_str().unwrap()]
    );
    assert!(search(&app, "ingredients=allium%20sativum")
        .await
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        search(&app, "exclude_ingredients=allium%20sativum")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let source = create(&app, "chilli", "Chilli prep").await["recipe"].clone();
    let chilli_id = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_ingredients",
        json!({}),
    )
    .await
    .as_array()
    .unwrap()
    .iter()
    .find(|row| row["name"] == "chilli")
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let merge = call(
        &app,
        "merge_ingredients",
        json!({"id":chilli_id,"survivor":garlic_id}),
    )
    .await;
    assert_ne!(merge["isError"], true);
    // Simulate a legacy merge-added alias: the redirect must outlive it.
    app.add_ingredient_alias_fixture("chilli", "chilli", &garlic_id)
        .await;
    assert_ne!(
        call(
            &app,
            "remove_ingredient_alias",
            json!({"id":garlic_id,"alias":"chilli"})
        )
        .await["isError"],
        true
    );
    assert_eq!(
        search(&app, "ingredients=chilli&sort=title")
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            used["id"].as_str().unwrap(),
            source["id"].as_str().unwrap(),
            garlic["id"].as_str().unwrap()
        ]
    );
    assert_eq!(
        search(&app, &format!("ingredients={chilli_id}"))
            .await
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let self_merge_by_id = call(
        &app,
        "merge_ingredients",
        json!({"id":garlic_id,"survivor":garlic_id}),
    )
    .await;
    assert_tool_error(&self_merge_by_id, "invalid_request");
    app.rebuild_derived().await;
    assert_eq!(
        search(&app, &format!("ingredients={chilli_id}"))
            .await
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn chained_merges_flatten_redirects_for_saves_search_counts_and_rebuild() {
    let app = support::TestApp::start().await;
    let recipe_a = create(&app, "ingredient a", "Recipe A").await["recipe"].clone();
    let recipe_b = create(&app, "ingredient b", "Recipe B").await["recipe"].clone();
    let recipe_c = create(&app, "ingredient c", "Recipe C").await["recipe"].clone();
    let ids = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_ingredients",
        json!({}),
    )
    .await;
    let ingredient_id = |name: &str| {
        ids.as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let a = ingredient_id("ingredient a");
    let b = ingredient_id("ingredient b");
    let c = ingredient_id("ingredient c");
    for (source, survivor) in [(&a, &b), (&b, &c)] {
        let merged = call(
            &app,
            "merge_ingredients",
            json!({"id":source,"survivor":survivor}),
        )
        .await;
        assert_ne!(merged["isError"], true);
    }

    let recipe_after_merges =
        create(&app, "ingredient a", "Recipe after merges").await["recipe"].clone();
    assert_eq!(
        search(&app, &format!("ingredients={a}"))
            .await
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let detail = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "get_ingredient",
        json!({"id":c}),
    )
    .await;
    assert_eq!(detail["usage_count"], 4);
    assert_eq!(detail["recipe_ids"].as_array().unwrap().len(), 4);

    app.rebuild_derived().await;
    for id in [&a, &b, &c] {
        assert_eq!(
            search(&app, &format!("ingredients={id}"))
                .await
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }
    assert_eq!(
        detail["recipe_ids"].as_array().unwrap().len(),
        [recipe_a, recipe_b, recipe_c, recipe_after_merges].len()
    );
}

#[tokio::test]
async fn structured_search_combines_filters_sorts_and_paginates_stably() {
    let app = support::TestApp::start().await;
    let alpha = create_document(
        &app,
        "Alpha soup",
        "bright broth",
        "smoky finish",
        2,
        &["weekday", "vegetarian"],
        "@tomato{100%g}\n@onion{1}",
    )
    .await["recipe"]
        .clone();
    let beta = create_document(
        &app,
        "Beta stew",
        "bright bowl",
        "pepper finish",
        4,
        &["weekday", "spicy"],
        "@tomato{100%g}\n@garlic{1}",
    )
    .await["recipe"]
        .clone();
    let gamma = create_document(
        &app,
        "Gamma rice",
        "plain side",
        "smoky finish",
        6,
        &["vegetarian"],
        "@rice{100%g}\n@onion{1}",
    )
    .await["recipe"]
        .clone();
    let delta = create_document(
        &app,
        "Delta bowl",
        "bright broth",
        "fresh",
        3,
        &["weekday", "vegetarian"],
        "@tomato{100%g}",
    )
    .await["recipe"]
        .clone();

    let ids = |response: Value| {
        response
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let combined = search(
        &app,
        "q=bright&ingredients=tomato,onion&tags=weekday,vegetarian",
    )
    .await;
    assert_eq!(ids(combined.clone()), vec![alpha["id"].as_str().unwrap()]);
    assert_eq!(
        combined[0]["matched_ingredients"],
        json!(["tomato", "onion"])
    );
    assert_eq!(
        ids(search(&app, "ingredients=tomato,garlic&ingredient_mode=any").await),
        vec![
            alpha["id"].as_str().unwrap(),
            beta["id"].as_str().unwrap(),
            delta["id"].as_str().unwrap()
        ]
    );
    assert_eq!(
        ids(search(&app, "tags=vegetarian,spicy&tag_mode=any").await),
        vec![
            alpha["id"].as_str().unwrap(),
            beta["id"].as_str().unwrap(),
            delta["id"].as_str().unwrap(),
            gamma["id"].as_str().unwrap()
        ]
    );
    assert_eq!(
        ids(search(
            &app,
            "tags=weekday&exclude_tags=spicy&exclude_ingredients=onion"
        )
        .await),
        vec![delta["id"].as_str().unwrap()]
    );
    assert_eq!(
        ids(search(&app, "q=smoky&min_servings=2&max_servings=3").await),
        vec![alpha["id"].as_str().unwrap()]
    );
    assert_eq!(
        ids(search(&app, "sort=title&limit=2&offset=1").await),
        vec![beta["id"].as_str().unwrap(), delta["id"].as_str().unwrap()]
    );
    assert_eq!(
        ids(search(&app, "sort=title&limit=2&offset=1").await),
        ids(search(&app, "sort=title&limit=2&offset=1").await)
    );
    assert_eq!(
        ids(search(&app, "q=Alpha&sort=relevance").await),
        vec![alpha["id"].as_str().unwrap()]
    );
    assert_eq!(
        ids(search(&app, "q=garlic").await),
        vec![beta["id"].as_str().unwrap()]
    );
    assert!(search(
        &app,
        "ingredients=tomato,unknown-ingredient&ingredient_mode=all"
    )
    .await
    .as_array()
    .unwrap()
    .is_empty());
    app.set_recipe_updated_at(alpha["id"].as_str().unwrap(), "2025-01-01T11:00:00Z")
        .await;
    app.set_recipe_updated_at(beta["id"].as_str().unwrap(), "2025-01-01T13:00:00+01:00")
        .await;
    app.set_recipe_updated_at(gamma["id"].as_str().unwrap(), "2025-01-01T12:00:00Z")
        .await;
    app.set_recipe_updated_at(delta["id"].as_str().unwrap(), "2025-01-01T15:00:00+02:00")
        .await;
    assert_eq!(
        ids(search(
            &app,
            "sort=updated&updated_since=2025-01-01T14%3A00%3A00%2B02%3A00"
        )
        .await),
        vec![
            delta["id"].as_str().unwrap(),
            beta["id"].as_str().unwrap(),
            gamma["id"].as_str().unwrap(),
        ]
    );
    assert_eq!(
        ids(search(&app, "sort=updated").await),
        vec![
            delta["id"].as_str().unwrap(),
            beta["id"].as_str().unwrap(),
            gamma["id"].as_str().unwrap(),
            alpha["id"].as_str().unwrap(),
        ]
    );

    let deleted = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "delete_recipe",
        json!({
            "id":alpha["id"],"base_revision":alpha["revision"]
        }),
    )
    .await;
    assert_eq!(deleted["deleted"], true);
    assert_eq!(ids(search(&app, "q=Alpha").await), Vec::<String>::new());
    assert_eq!(
        ids(search(&app, "q=Alpha&include_deleted=true").await),
        vec![alpha["id"].as_str().unwrap()]
    );
    let restored = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "restore_recipe",
        json!({
            "id":alpha["id"],"base_revision":deleted["revision"]
        }),
    )
    .await;
    assert_eq!(restored["deleted"], false);
    app.rebuild_derived().await;
    assert_eq!(
        ids(search(&app, "q=Alpha").await),
        vec![alpha["id"].as_str().unwrap()]
    );
}

#[tokio::test]
async fn ingredient_normalization_uses_unicode_default_case_folding() {
    let app = support::TestApp::start().await;
    let recipe = create(&app, "Straße", "Street bread").await["recipe"].clone();
    let results = search(&app, "ingredients=STRASSE").await;
    assert_eq!(results.as_array().unwrap().len(), 1);
    assert_eq!(results[0]["id"], recipe["id"]);
}

#[tokio::test]
async fn ingredient_mutations_reject_missing_arguments() {
    let app = support::TestApp::start().await;
    let recipe = create(&app, "salt", "Salt").await["recipe"].clone();
    let ingredients = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_ingredients",
        json!({}),
    )
    .await;
    let id = ingredients[0]["id"].as_str().unwrap();
    let response = call(&app, "rename_ingredient", json!({"id":id})).await;
    assert_eq!(response["isError"], true);
    assert!(response["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("name"));
    let detail = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "get_ingredient",
        json!({"id":id}),
    )
    .await;
    assert_eq!(detail["name"], "salt");
    assert_eq!(recipe["title"], "Salt");
}

#[tokio::test]
async fn delete_and_both_restore_paths_copy_saved_alias_resolution() {
    let app = support::TestApp::start().await;
    let created = create(&app, "oregano", "Herb recipe").await["recipe"].clone();
    let id = created["id"].as_str().unwrap();
    let ingredients = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_ingredients",
        json!({}),
    )
    .await;
    let oregano_id = ingredients[0]["id"].as_str().unwrap().to_owned();
    let add_alias = call(
        &app,
        "add_ingredient_alias",
        json!({"id":oregano_id,"alias":"wild marjoram"}),
    )
    .await;
    assert_ne!(add_alias["isError"], true);

    let draft = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "save_draft",
        json!({
            "recipe_id":id,"document":{"body":"@wild marjoram{10%g}"}
        }),
    )
    .await;
    let updated = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "publish_draft",
        json!({
            "id":draft["id"],"expected_revision":draft["revision"]
        }),
    )
    .await["recipe"]
        .clone();
    let alias_revision = updated["revision"].as_str().unwrap();
    assert_eq!(
        search(&app, &format!("ingredients={oregano_id}"))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ne!(
        call(
            &app,
            "remove_ingredient_alias",
            json!({"id":oregano_id,"alias":"wild marjoram"})
        )
        .await["isError"],
        true
    );

    let deleted = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "delete_recipe",
        json!({
            "id":id,"base_revision":alias_revision
        }),
    )
    .await;
    let restored = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "restore_recipe",
        json!({
            "id":id,"base_revision":deleted["revision"]
        }),
    )
    .await;
    let historical = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "restore_revision",
        json!({
            "id":id,"revision":alias_revision,"base_revision":restored["revision"]
        }),
    )
    .await;
    assert_eq!(historical["body"], "@wild marjoram{10%g}");
    app.rebuild_derived().await;

    assert_eq!(
        search(&app, &format!("ingredients={oregano_id}"))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(search(&app, "ingredients=wild%20marjoram")
        .await
        .as_array()
        .unwrap()
        .is_empty());
    let history = mcp::tool(
        &app.client(),
        &app.url(""),
        "test-secret",
        "list_revisions",
        json!({"id":id}),
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 5);
}
