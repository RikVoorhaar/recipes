use recipe_app::{app_with_storage, configure_tokens, database, storage::FileImageStorage};
use serde_json::{json, Value};
use std::{io::Cursor, sync::Arc, time::Duration};
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
        "create_recipe",
        "update_recipe",
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
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert!(tool["description"]
            .as_str()
            .unwrap()
            .contains("get_guidelines"));
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
        json!({"name":"create_recipe","arguments":{
            "title":"MCP test dish","servings":2,"body":"Add @tomato{100%g}.","tags":["stdio"]
        }}),
    )
    .await;
    assert_eq!(create["result"]["isError"], Value::Null);
    let saved: Value =
        serde_json::from_str(create["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
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
        json!({"name":"create_recipe","arguments":{
            "title":"Invalid MCP body","servings":1,"body":"First step\n~rest{}"
        }}),
    )
    .await;
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
    let attached: Value =
        serde_json::from_str(attached["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let new_revision = attached["revision"].as_str().unwrap();

    let update = rpc(&mut input, &mut output, 7, "tools/call", json!({"name":"update_recipe","arguments":{
        "id":id,"base_revision":new_revision,"title":"MCP updated","servings":2,"body":"Add @tomato{100%g}.","tags":["stdio"]
    }})).await;
    assert_ne!(update["result"]["isError"], true);
    let stale = rpc(&mut input, &mut output, 8, "tools/call", json!({"name":"update_recipe","arguments":{
        "id":id,"base_revision":new_revision,"title":"Stale","servings":2,"body":"Add @tomato{100%g}."
    }})).await;
    assert_eq!(stale["result"]["isError"], true);
    assert!(stale["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("current_revision_id"));

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
        json!({"name":"create_recipe","arguments":{
            "title":"Garlic side","servings":1,"body":"Use @garlic{1%clove}."
        }}),
    )
    .await;
    let _second_recipe: Value = serde_json::from_str(
        second_recipe["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
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
