//! Transport-independent JSON-RPC dispatch for an authenticated MCP caller.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::{
    add_alias, append_image_revision, append_state_revision, attach_image, discard_draft,
    generate_draft_cover, generate_recipe_cover, get_draft, get_draft_cover, get_ingredient,
    get_recipe, get_revision, list_drafts, list_ingredients, list_recipes, list_revisions,
    list_tags, merge_ingredient, publish_draft, remove_alias, rename_ingredient, save_draft,
    search_recipes, validate_recipe, AliasBody, ApiError, AttachImage, CallerIdentity,
    GenerateDraftCover, GenerateRecipeCover, IngredientQuery, MergeBody, NameBody, RecipeService,
    SearchQuery, GENERATED_COVER_MIME, RECIPE_GUIDELINES,
};

fn tools() -> Value {
    let names = [
        ("search_recipes", "Search recipes using the API structured filters in query.", ""),
        ("list_recipes", "List current recipes.", ""),
        ("get_recipe", "Get a recipe by ID.", "id"),
        ("save_draft", "Start or revise an owner-private draft. Existing drafts require id and expected_revision; use recipe_id to start an edit.", "document"),
        ("get_draft", "Read one of your private drafts.", "id"),
        ("list_drafts", "List your private drafts.", ""),
        ("discard_draft", "Discard a draft using its expected_revision.", "id,expected_revision"),
        ("publish_draft", "Validate and explicitly publish a draft using its expected_revision; optionally select its generated image_ref. Publishing consumes it.", "id,expected_revision"),
        ("generate_cover", "By default make one cover attempt for a new dish unless the user opts out. Provide exactly one target: draft_id with expected_revision for a private candidate, or recipe_id with base_revision and optional replace for immediate attachment. Inspect a draft preview before explicitly publishing; if generation fails, report it and proceed without a cover when publication was requested.", ""),
        ("delete_recipe", "Soft-delete a recipe using its current base_revision.", "id,base_revision"),
        ("restore_recipe", "Restore a soft-deleted recipe using its current base_revision.", "id,base_revision"),
        ("list_revisions", "List a recipe's immutable revisions.", "id"),
        ("get_revision", "Get one immutable recipe revision.", "id,revision"),
        ("restore_revision", "Restore a historical revision as a new current revision; base_revision is required.", "id,revision,base_revision"),
        ("attach_image", "Attach or replace an uploaded image, requiring base_revision.", "id,base_revision,image_ref"),
        ("detach_image", "Detach the image, requiring base_revision.", "id,base_revision"),
        ("list_ingredients", "List/search canonical ingredients.", ""),
        ("get_ingredient", "Get ingredient details and linked recipes.", "id"),
        ("add_ingredient_alias", "Add an alias to an ingredient.", "id,alias"),
        ("remove_ingredient_alias", "Remove an ingredient alias.", "id,alias"),
        ("rename_ingredient", "Rename a canonical ingredient.", "id,name"),
        ("merge_ingredients", "Merge this ingredient into a survivor.", "id,survivor"),
        ("list_tags", "List tags with recipe counts.", ""),
        ("validate_recipe", "Dry-run Cooklang validation and print-fit check without saving.", "title,servings,body"),
        ("get_guidelines", "Get recipe-writing guidelines. Read before drafting. Workflow: read guidelines, save a draft, read it back and validate, then explicitly publish.", ""),
    ];
    Value::Array(
        names
            .iter()
            .map(|(name, description, required)| {
                let required: Vec<&str> = required.split(',').filter(|s| !s.is_empty()).collect();
                json!({
                    "name":name,
                    "description":description,
                    "inputSchema":tool_schema(name, &required)
                })
            })
            .collect(),
    )
}

fn tool_schema(name: &str, required: &[&str]) -> Value {
    let recipe_fields = json!({
        "id":{"type":"string"},
        "recipe_id":{"type":"string","description":"Published recipe ID to start an edit draft."},
        "expected_revision":{"type":"integer"},
        "document":{"type":"object","description":"Draft fields; incomplete documents are allowed."},
        "revision":{"type":"string"},
        "base_revision":{"type":"string"},
        "title":{"type":"string"},
        "description":{"type":"string"},
        "servings":{"type":"integer","minimum":1},
        "body":{"type":"string"},
        "tags":{"type":"array","items":{"type":"string"}},
        "source_url":{"type":["string","null"],"format":"uri"},
        "notes":{"type":["string","null"]},
        "path":{"type":"string"},
        "image_ref":{"type":"string"},
        "draft_id":{"type":"string"},
        "dish_description":{"type":"string","description":"Short description of the finished dish."},
        "replace":{"type":"boolean","default":false},
        "alias":{"type":"string"},
        "name":{"type":"string"},
        "survivor":{"type":"string"}
    });
    let search_filters = json!({
        "q":{"type":"string","description":"Substring search across title, description, notes and body."},
        "ingredients":{"type":"string","description":"Comma-separated canonical IDs, names or aliases."},
        "ingredient_mode":{"type":"string","enum":["all","any"]},
        "exclude_ingredients":{"type":"string","description":"Comma-separated canonical IDs, names or aliases to exclude."},
        "tags":{"type":"string","description":"Comma-separated tags."},
        "tag_mode":{"type":"string","enum":["all","any"]},
        "exclude_tags":{"type":"string","description":"Comma-separated tags to exclude."},
        "min_servings":{"type":"integer","minimum":1},
        "max_servings":{"type":"integer","minimum":1},
        "has_image":{"type":"boolean"},
        "include_deleted":{"type":"boolean","default":false},
        "updated_since":{"type":"string","format":"date-time"},
        "sort":{"type":"string","enum":["relevance","title","updated"]},
        "limit":{"type":"integer","minimum":1,"maximum":200},
        "offset":{"type":"integer","minimum":0}
    });
    match name {
        "search_recipes" => json!({
            "type":"object",
            "properties":{"query":{
                "type":"object",
                "description":"Optional API search filters; comma-separated list filters use strings.",
                "properties":search_filters,
                "additionalProperties":false
            }},
            "additionalProperties":false
        }),
        "validate_recipe" => json!({
            "type":"object",
            "properties":{
                "title":{"type":"string"},
                "description":{"type":"string"},
                "servings":{"type":"integer","minimum":1},
                "tags":{"type":"array","items":{"type":"string"}},
                "notes":{"type":["string","null"]},
                "source_url":{"type":["string","null"],"format":"uri"},
                "image_ref":{"type":["string","null"],"description":"Optional opaque uploaded image ref to validate as the candidate attachment."},
                "body":{"type":"string"},
                "target_servings":{"type":"integer","minimum":1,"maximum":1000,"description":"Optional servings count for the non-mutating scaled preview; defaults to servings."}
            },
            "required":["title","servings","body"],
            "additionalProperties":false
        }),
        "generate_cover" => json!({
            "type":"object",
            "properties":{
                "draft_id":{"type":"string"},
                "expected_revision":{"type":"integer","minimum":1},
                "recipe_id":{"type":"string"},
                "base_revision":{"type":"string"},
                "replace":{"type":"boolean","default":false},
                "dish_description":{"type":"string","description":"Short finished-dish description, at most 500 Unicode characters.","minLength":1,"maxLength":500}
            },
            "required":["dish_description"],
            "oneOf":[
                {"required":["draft_id","expected_revision"],"not":{"anyOf":[{"required":["recipe_id"]},{"required":["base_revision"]},{"required":["replace"]}]}},
                {"required":["recipe_id","base_revision"],"not":{"anyOf":[{"required":["draft_id"]},{"required":["expected_revision"]}]}}
            ],
            "additionalProperties":false
        }),
        "list_ingredients" => json!({
            "type":"object",
            "properties":{"q":{"type":"string","description":"Optional canonical ingredient name search."}},
            "additionalProperties":false
        }),
        _ => json!({
            "type":"object",
            "properties":recipe_fields,
            "required":required,
            "additionalProperties":true
        }),
    }
}

fn arguments<T: DeserializeOwned>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|error| format!("invalid arguments: {error}"))
}

fn required_text<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required argument: {key}"))
}

fn json_text(value: impl Serialize) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|_| "could not encode the tool result".into())
}

fn service_error(error: ApiError) -> String {
    json_text(&error).unwrap_or(error.message)
}

fn result_text<T: Serialize>(result: Result<T, ApiError>) -> Result<String, String> {
    json_text(result.map_err(service_error)?)
}

fn search_query(value: Option<&Value>) -> Result<SearchQuery, String> {
    let Some(value) = value else {
        return Ok(SearchQuery::default());
    };
    let fields = value
        .as_object()
        .ok_or("query must be an object of named search filters")?;
    for (key, value) in fields {
        let valid = match key.as_str() {
            "q"
            | "ingredients"
            | "exclude_ingredients"
            | "tags"
            | "exclude_tags"
            | "updated_since" => value.is_string(),
            "ingredient_mode" | "tag_mode" => matches!(value.as_str(), Some("all" | "any")),
            "sort" => matches!(value.as_str(), Some("relevance" | "title" | "updated")),
            "min_servings" | "max_servings" | "limit" | "offset" => value.is_u64(),
            "has_image" | "include_deleted" => value.is_boolean(),
            _ => return Err(format!("unsupported search filter: {key}")),
        };
        if !valid {
            return Err(format!("invalid value for search filter: {key}"));
        }
    }
    arguments(value)
}

fn ingredient_query(args: &Value) -> Result<IngredientQuery, String> {
    let fields = args
        .as_object()
        .ok_or("ingredient-list arguments must be an object")?;
    if let Some(key) = fields.keys().find(|key| key.as_str() != "q") {
        return Err(format!("unsupported ingredient-list parameter: {key}"));
    }
    let q = args
        .get("q")
        .map(|value| value.as_str().ok_or("q must be a string"))
        .transpose()?;
    Ok(IngredientQuery {
        q: q.map(str::to_owned),
    })
}

async fn invoke(
    service: &RecipeService,
    caller: &CallerIdentity,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let id = || required_text(args, "id");
    let base = || required_text(args, "base_revision");
    match name {
        "get_guidelines" => Ok(RECIPE_GUIDELINES.into()),
        "save_draft" => result_text(save_draft(service, caller, arguments(args)?).await),
        "get_draft" => result_text(get_draft(service, caller, id()?).await),
        "list_drafts" => result_text(list_drafts(service, caller).await),
        "discard_draft" => {
            discard_draft(service, caller, id()?, arguments(args)?)
                .await
                .map_err(service_error)?;
            Ok(String::new())
        }
        "publish_draft" => {
            result_text(publish_draft(service, caller, id()?, arguments(args)?).await)
        }
        "search_recipes" => {
            result_text(search_recipes(service, search_query(args.get("query"))?).await)
        }
        "list_recipes" => result_text(list_recipes(service).await),
        "get_recipe" => result_text(get_recipe(service, id()?).await),
        "delete_recipe" | "restore_recipe" | "restore_revision" => result_text(
            append_state_revision(
                service,
                caller,
                id()?,
                base()?,
                name == "delete_recipe",
                if name == "restore_revision" {
                    Some(required_text(args, "revision")?)
                } else {
                    None
                },
            )
            .await,
        ),
        "list_revisions" => result_text(list_revisions(service, id()?).await),
        "get_revision" => {
            result_text(get_revision(service, id()?, required_text(args, "revision")?).await)
        }
        "attach_image" => result_text(
            attach_image(
                service,
                caller,
                id()?,
                AttachImage {
                    base_revision: base()?.into(),
                    image_ref: required_text(args, "image_ref")?.into(),
                },
            )
            .await,
        ),
        "detach_image" => {
            result_text(append_image_revision(service, caller, id()?, base()?, None).await)
        }
        "list_ingredients" => result_text(list_ingredients(service, ingredient_query(args)?).await),
        "get_ingredient" => result_text(get_ingredient(service, id()?).await),
        "add_ingredient_alias" => {
            add_alias(
                service,
                caller,
                id()?,
                AliasBody {
                    alias: required_text(args, "alias")?.into(),
                },
            )
            .await
            .map_err(service_error)?;
            Ok(String::new())
        }
        "remove_ingredient_alias" => {
            remove_alias(service, caller, id()?, required_text(args, "alias")?)
                .await
                .map_err(service_error)?;
            Ok(String::new())
        }
        "rename_ingredient" => {
            rename_ingredient(
                service,
                caller,
                id()?,
                NameBody {
                    name: required_text(args, "name")?.into(),
                },
            )
            .await
            .map_err(service_error)?;
            Ok(String::new())
        }
        "merge_ingredients" => {
            merge_ingredient(
                service,
                caller,
                id()?,
                MergeBody {
                    survivor: required_text(args, "survivor")?.into(),
                },
            )
            .await
            .map_err(service_error)?;
            Ok(String::new())
        }
        "list_tags" => result_text(list_tags(service).await),
        "validate_recipe" => result_text(validate_recipe(service, arguments(args)?).await),
        _ => Err(format!("unknown tool: {name}")),
    }
}

async fn invoke_generate_cover(
    service: &RecipeService,
    caller: &CallerIdentity,
    args: &Value,
) -> Result<Vec<Value>, String> {
    let fields = args
        .as_object()
        .ok_or("generate_cover arguments must be an object")?;
    let has = |key| fields.contains_key(key);
    let allowed = [
        "draft_id",
        "expected_revision",
        "recipe_id",
        "base_revision",
        "replace",
        "dish_description",
    ];
    if let Some(field) = fields
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("unsupported generate_cover argument: {field}"));
    }
    if has("draft_id") == has("recipe_id") {
        return Err("provide exactly one target: draft_id or recipe_id".into());
    }
    let dish = args["dish_description"]
        .as_str()
        .filter(|dish| {
            !dish.trim().is_empty()
                && dish.chars().count() <= crate::cover::MAX_DISH_DESCRIPTION_CHARS
        })
        .ok_or("dish_description must be a non-empty string of at most 500 characters")?;
    if has("draft_id") {
        if has("base_revision") || has("replace") {
            return Err("draft and published recipe targets are mutually exclusive".into());
        }
        let id = args["draft_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("draft_id must be a non-empty string")?;
        let expected_revision = args["expected_revision"]
            .as_i64()
            .filter(|revision| *revision > 0)
            .ok_or("draft target requires a positive expected_revision")?;
        let candidate = generate_draft_cover(
            service,
            caller,
            id,
            GenerateDraftCover {
                expected_revision,
                dish_description: dish.into(),
            },
        )
        .await
        .map_err(service_error)?;
        // Look up the image with the same caller and draft, never via the preview URL.
        let image = get_draft_cover(service, caller, id, &candidate.image_ref)
            .await
            .map_err(|_| "could not retrieve authenticated cover preview")?;
        if image.len() > 5 * 1024 * 1024 {
            return Err("cover preview exceeds the 5 MiB MCP limit".into());
        }
        if image.is_empty() {
            return Err("cover preview is empty".into());
        }
        Ok(vec![
            text_content(json_text(candidate)?),
            json!({"type":"image","data":STANDARD.encode(image),"mimeType":GENERATED_COVER_MIME}),
        ])
    } else {
        if has("expected_revision") {
            return Err("draft and published recipe targets are mutually exclusive".into());
        }
        let id = args["recipe_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("recipe_id must be a non-empty string")?;
        let base_revision = args["base_revision"]
            .as_str()
            .filter(|revision| !revision.is_empty())
            .ok_or("published recipe target requires base_revision")?;
        let replace = match args.get("replace") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err("replace must be a boolean".into()),
        };
        let text = result_text(
            generate_recipe_cover(
                service,
                caller,
                id,
                GenerateRecipeCover {
                    base_revision: base_revision.into(),
                    dish_description: dish.into(),
                    replace,
                },
            )
            .await,
        )?;
        Ok(vec![text_content(text)])
    }
}

fn text_content(text: String) -> Value {
    json!({"type":"text","text":text})
}

const GUIDELINES_URI: &str = "recipe://guidelines";
const INSTRUCTIONS: &str = "Recipe authoring workflow: call get_guidelines first, save or revise an owner-private draft, read it back with get_draft, validate the candidate, and only then explicitly call publish_draft. By default make one generate_cover attempt for a new dish unless the user opts out; inspect the private preview before explicitly publishing. If generation fails, report it and publish without a cover when requested. Drafts persist across sessions. Do not use HTTP recipe create/update routes for agent authoring.";

/// Dispatch a decoded message after the transport has authenticated the caller.
/// Notifications have no response.
pub(crate) async fn handle(
    service: &RecipeService,
    caller: &CallerIdentity,
    message: Value,
) -> Option<Value> {
    let id = message.get("id")?;
    let params = &message["params"];
    let error =
        |code, message| json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}});
    let result = match message["method"].as_str() {
        Some("initialize") => json!({
            "protocolVersion":negotiate_protocol(params.get("protocolVersion")),
            "capabilities":{"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false}},
            "instructions":INSTRUCTIONS,
            "serverInfo":{"name":"recipe-mcp","version":"0.1.0"}
        }),
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools":tools()}),
        Some("resources/list") => {
            json!({"resources":[{"uri":GUIDELINES_URI,"name":"Recipe writing guidelines","mimeType":"text/markdown","description":"Required authoring guidance; read before creating or updating recipes."}]})
        }
        Some("resources/read") if params["uri"] == GUIDELINES_URI => {
            json!({"contents":[{"uri":GUIDELINES_URI,"mimeType":"text/markdown","text":RECIPE_GUIDELINES}]})
        }
        Some("resources/read") => return Some(error(-32602, "unknown resource URI")),
        Some("tools/call") => {
            let name = params["name"].as_str().unwrap_or("");
            let empty = json!({});
            let args = params.get("arguments").unwrap_or(&empty);
            let content = if name == "generate_cover" {
                invoke_generate_cover(service, caller, args).await
            } else {
                invoke(service, caller, name, args)
                    .await
                    .map(|text| vec![text_content(text)])
            };
            match content {
                Ok(content) => json!({"content":content}),
                Err(error) => json!({"content":[text_content(error)],"isError":true}),
            }
        }
        _ => return Some(error(-32601, "method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

const SUPPORTED_PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

fn negotiate_protocol(requested: Option<&Value>) -> &'static str {
    requested
        .and_then(Value::as_str)
        .and_then(|version| {
            SUPPORTED_PROTOCOLS
                .iter()
                .copied()
                .find(|supported| *supported == version)
        })
        .unwrap_or(SUPPORTED_PROTOCOLS[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{authorize_identity, configure_tokens, database, FileImageStorage};
    use axum::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};
    use std::sync::Arc;

    async fn service(
        dir: &std::path::Path,
        generator: Option<crate::cover::Generator>,
    ) -> RecipeService {
        let pool = database::connect(&dir.join("recipes.sqlite"))
            .await
            .unwrap();
        database::migrate(&pool).await.unwrap();
        configure_tokens(
            &pool,
            &["cook:owner-secret".into(), "cook:other-secret".into()],
        )
        .await
        .unwrap();
        RecipeService::new(
            pool,
            Arc::new(FileImageStorage::new(dir.join("images"))),
            generator.map(Arc::new),
        )
    }

    async fn caller(service: &RecipeService, secret: &'static str) -> CallerIdentity {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static(secret));
        authorize_identity(service, &headers).await.unwrap()
    }

    async fn rpc(
        service: &RecipeService,
        caller: &CallerIdentity,
        method: &str,
        params: Value,
    ) -> Value {
        let response = handle(
            service,
            caller,
            json!({"jsonrpc":"2.0","id":"request-1","method":method,"params":params}),
        )
        .await
        .unwrap();
        assert_eq!(response["id"], "request-1");
        assert_eq!(response["jsonrpc"], "2.0");
        response
    }

    async fn tool(
        service: &RecipeService,
        caller: &CallerIdentity,
        name: &str,
        args: Value,
    ) -> Value {
        rpc(
            service,
            caller,
            "tools/call",
            json!({"name":name,"arguments":args}),
        )
        .await["result"]
            .clone()
    }

    fn result_json(result: &Value) -> Value {
        assert_ne!(result["isError"], true, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn protocol_negotiation_and_discovery_work_without_a_transport() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path(), None).await;
        let owner = caller(&service, "Bearer owner-secret").await;

        for version in SUPPORTED_PROTOCOLS {
            let initialized = rpc(
                &service,
                &owner,
                "initialize",
                json!({"protocolVersion":version}),
            )
            .await;
            assert_eq!(&initialized["result"]["protocolVersion"], *version);
            assert_eq!(initialized["result"]["instructions"], INSTRUCTIONS);
        }
        assert_eq!(
            rpc(
                &service,
                &owner,
                "initialize",
                json!({"protocolVersion":"future"})
            )
            .await["result"]["protocolVersion"],
            SUPPORTED_PROTOCOLS[0]
        );
        assert!(handle(
            &service,
            &owner,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .is_none());

        let listed = rpc(&service, &owner, "tools/list", json!({})).await;
        let catalog = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(catalog.len(), 25);
        assert!(!catalog.iter().any(|tool| tool["name"] == "upload_image"));
        let removed = tool(
            &service,
            &owner,
            "upload_image",
            json!({"path":"/not-read.jpg"}),
        )
        .await;
        assert_eq!(removed["isError"], true);
        assert_eq!(removed["content"][0]["text"], "unknown tool: upload_image");

        let resource = rpc(
            &service,
            &owner,
            "resources/read",
            json!({"uri":GUIDELINES_URI}),
        )
        .await;
        let guidelines = tool(&service, &owner, "get_guidelines", json!({})).await;
        assert_eq!(
            resource["result"]["contents"][0]["text"],
            guidelines["content"][0]["text"]
        );
        let error = rpc(
            &service,
            &owner,
            "resources/read",
            json!({"uri":"recipe://unknown"}),
        )
        .await;
        assert_eq!(error["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn drafts_use_the_authenticated_token_for_ownership_and_attribution() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path(), None).await;
        let owner = caller(&service, "Bearer owner-secret").await;
        let other = caller(&service, "Bearer other-secret").await;
        let draft = result_json(&tool(
            &service,
            &owner,
            "save_draft",
            json!({"document":{"title":"Soup","servings":2,"body":"Heat @water{1%l}.","author_label":"forged"}}),
        ).await);
        let id = &draft["id"];
        assert_eq!(
            result_json(&tool(&service, &other, "list_drafts", json!({})).await),
            json!([])
        );
        assert_eq!(
            tool(&service, &other, "get_draft", json!({"id":id})).await["isError"],
            true
        );
        let publish = json!({"id":id,"expected_revision":draft["revision"]});
        assert_eq!(
            tool(&service, &other, "publish_draft", publish.clone()).await["isError"],
            true
        );
        assert_eq!(
            result_json(&tool(&service, &owner, "get_draft", json!({"id":id})).await),
            draft
        );
        let published = result_json(&tool(&service, &owner, "publish_draft", publish).await);
        assert_eq!(published["recipe"]["author_label"], "cook");
        assert_eq!(
            result_json(&tool(&service, &owner, "list_drafts", json!({})).await),
            json!([])
        );
        let hits = result_json(
            &tool(
                &service,
                &other,
                "search_recipes",
                json!({"query":{"ingredients":"water","min_servings":2,"has_image":false}}),
            )
            .await,
        );
        assert_eq!(hits[0]["id"], published["recipe"]["id"]);
        let invalid = tool(
            &service,
            &owner,
            "search_recipes",
            json!({"query":{"unknown":true}}),
        )
        .await;
        assert_eq!(invalid["isError"], true);
    }

    #[tokio::test]
    async fn generated_draft_cover_returns_private_jpeg_content() {
        use axum::{routing::post, Json, Router};
        use image::{ImageBuffer, ImageFormat, Rgb};
        use std::io::Cursor;

        // The only HTTP listener is the fake image provider, not the recipe app.
        let mut png = Cursor::new(Vec::new());
        ImageBuffer::from_pixel(2, 2, Rgb([180_u8, 80, 20]))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let encoded = STANDARD.encode(png.into_inner());
        let provider = Router::new().route(
            "/images",
            post(move || {
                let encoded = encoded.clone();
                async move { Json(json!({"data":[{"b64_json":encoded}]})) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/images", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
        let generator =
            crate::cover::Generator::with_test_endpoint("test-key".into(), None, None, &endpoint)
                .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path(), Some(generator)).await;
        let owner = caller(&service, "Bearer owner-secret").await;
        let other = caller(&service, "Bearer other-secret").await;
        let draft =
            result_json(&tool(&service, &owner, "save_draft", json!({"document":{}})).await);
        let args = json!({"draft_id":draft["id"],"expected_revision":1,"dish_description":"A bowl of soup"});
        assert_eq!(
            tool(&service, &other, "generate_cover", args.clone()).await["isError"],
            true
        );

        let result = tool(&service, &owner, "generate_cover", args).await;
        let candidate = result_json(&result);
        let image = &result["content"][1];
        assert_eq!(image["type"], "image");
        assert_eq!(image["mimeType"], "image/jpeg");
        let bytes = STANDARD.decode(image["data"].as_str().unwrap()).unwrap();
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Jpeg);
        assert!(get_draft_cover(
            &service,
            &other,
            draft["id"].as_str().unwrap(),
            candidate["image_ref"].as_str().unwrap(),
        )
        .await
        .is_err());
    }
}
