use reqwest::{header, Client, Method, Response};
use serde_json::{json, Value};
use std::{
    env,
    fs::{File, OpenOptions},
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};

#[derive(Clone)]
struct Api {
    client: Client,
    base: String,
}

impl Api {
    fn new() -> Result<Self, String> {
        let base = env::var("RECIPE_API_BASE_URL")
            .map_err(|_| "RECIPE_API_BASE_URL is required".to_string())?;
        let token =
            env::var("RECIPE_API_TOKEN").map_err(|_| "RECIPE_API_TOKEN is required".to_string())?;
        let mut headers = header::HeaderMap::new();
        let value = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "RECIPE_API_TOKEN is invalid".to_string())?;
        headers.insert(header::AUTHORIZATION, value);
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "could not initialize HTTP client".to_string())?;
        Ok(Self {
            client,
            base: base.trim_end_matches('/').to_string(),
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Response, String> {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
            .send()
            .await
            .map_err(|_| "API request failed; check the API URL and connectivity".to_string())
    }
}

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
        ("upload_image", "Upload an image from a local file path; returns an opaque image_ref.", "path"),
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

async fn invoke(api: &Api, name: &str, args: &Value) -> Result<String, String> {
    if name == "get_guidelines" {
        let r = api.request(Method::GET, "/api/v1/guidelines", None).await?;
        return api_text(r).await;
    }
    let text = |key: &str| -> Result<String, String> {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("missing required argument: {key}"))
    };
    let (method, path, body) = match name {
        "save_draft" => (Method::POST, "/api/v1/drafts".into(), Some(args.clone())),
        "get_draft" => (Method::GET, format!("/api/v1/drafts/{}", text("id")?), None),
        "list_drafts" => (Method::GET, "/api/v1/drafts".into(), None),
        "discard_draft" => (
            Method::DELETE,
            format!("/api/v1/drafts/{}", text("id")?),
            Some(json!({"expected_revision":args["expected_revision"]})),
        ),
        "publish_draft" => (
            Method::POST,
            format!("/api/v1/drafts/{}/publish", text("id")?),
            Some({
                let mut body = json!({"expected_revision":args["expected_revision"]});
                if let Some(image_ref) = args.get("image_ref") {
                    body["image_ref"] = image_ref.clone();
                }
                body
            }),
        ),
        "search_recipes" => (
            Method::GET,
            format!("/api/v1/search{}", search_query_string(args.get("query"))?),
            None,
        ),
        "list_recipes" => (Method::GET, "/api/v1/recipes".into(), None),
        "get_recipe" => (
            Method::GET,
            format!("/api/v1/recipes/{}", text("id")?),
            None,
        ),
        "delete_recipe" => (
            Method::DELETE,
            format!("/api/v1/recipes/{}", text("id")?),
            Some(json!({"base_revision":text("base_revision")?})),
        ),
        "restore_recipe" => (
            Method::POST,
            format!("/api/v1/recipes/{}/restore", text("id")?),
            Some(json!({"base_revision":text("base_revision")?})),
        ),
        "list_revisions" => (
            Method::GET,
            format!("/api/v1/recipes/{}/revisions", text("id")?),
            None,
        ),
        "get_revision" => (
            Method::GET,
            format!(
                "/api/v1/recipes/{}/revisions/{}",
                text("id")?,
                text("revision")?
            ),
            None,
        ),
        "restore_revision" => (
            Method::POST,
            format!(
                "/api/v1/recipes/{}/revisions/{}",
                text("id")?,
                text("revision")?
            ),
            Some(json!({"base_revision":text("base_revision")?})),
        ),
        "attach_image" => (
            Method::POST,
            format!("/api/v1/recipes/{}/image", text("id")?),
            Some(json!({"base_revision":text("base_revision")?,"image_ref":text("image_ref")?})),
        ),
        "detach_image" => (
            Method::DELETE,
            format!("/api/v1/recipes/{}/image", text("id")?),
            Some(json!({"base_revision":text("base_revision")?})),
        ),
        "list_ingredients" => {
            let parameters = args
                .as_object()
                .ok_or_else(|| "ingredient-list arguments must be an object".to_string())?;
            if let Some(key) = parameters.keys().find(|key| key.as_str() != "q") {
                return Err(format!("unsupported ingredient-list parameter: {key}"));
            }
            let q = args.get("q");
            let query = match q {
                None => String::new(),
                Some(Value::String(q)) => format!("?q={}", urlencoding(q)),
                Some(_) => return Err("q must be a string".into()),
            };
            (Method::GET, format!("/api/v1/ingredients{query}"), None)
        }
        "get_ingredient" => (
            Method::GET,
            format!("/api/v1/ingredients/{}", text("id")?),
            None,
        ),
        "add_ingredient_alias" => (
            Method::POST,
            format!("/api/v1/ingredients/{}/aliases", text("id")?),
            Some(json!({"alias":text("alias")?})),
        ),
        "remove_ingredient_alias" => (
            Method::DELETE,
            format!(
                "/api/v1/ingredients/{}/aliases/{}",
                text("id")?,
                urlencoding(&text("alias")?)
            ),
            None,
        ),
        "rename_ingredient" => (
            Method::PUT,
            format!("/api/v1/ingredients/{}", text("id")?),
            Some(json!({"name":text("name")?})),
        ),
        "merge_ingredients" => (
            Method::POST,
            format!("/api/v1/ingredients/{}/merge", text("id")?),
            Some(json!({"survivor":text("survivor")?})),
        ),
        "list_tags" => (Method::GET, "/api/v1/tags".into(), None),
        "validate_recipe" => (
            Method::POST,
            "/api/v1/recipes/validate".into(),
            Some(args.clone()),
        ),
        "upload_image" => {
            let path = PathBuf::from(text("path")?);
            let (file, mime) = tokio::task::spawn_blocking(move || read_local_image(path))
                .await
                .map_err(|_| "could not read local image file".to_string())??;
            let response = api
                .client
                .post(format!("{}/api/v1/images", api.base))
                .header(header::CONTENT_TYPE, mime)
                .body(file)
                .send()
                .await
                .map_err(|_| {
                    "API request failed; check the API URL and connectivity".to_string()
                })?;
            return api_text(response).await;
        }
        _ => return Err(format!("unknown tool: {name}")),
    };
    api_text(api.request(method, &path, body).await?).await
}

async fn invoke_generate_cover(api: &Api, args: &Value) -> Result<Vec<Value>, String> {
    let fields = args
        .as_object()
        .ok_or_else(|| "generate_cover arguments must be an object".to_string())?;
    const ALLOWED_FIELDS: &[&str] = &[
        "draft_id",
        "expected_revision",
        "recipe_id",
        "base_revision",
        "replace",
        "dish_description",
    ];
    if let Some(field) = fields
        .keys()
        .find(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
    {
        return Err(format!("unsupported generate_cover argument: {field}"));
    }
    let has_draft = fields.contains_key("draft_id");
    let has_recipe = fields.contains_key("recipe_id");
    if has_draft == has_recipe {
        return Err("provide exactly one target: draft_id or recipe_id".into());
    }
    let draft = args.get("draft_id").and_then(Value::as_str);
    let recipe = args.get("recipe_id").and_then(Value::as_str);
    if (has_draft && draft.is_none()) || (has_recipe && recipe.is_none()) {
        return Err("target ID must be a string".into());
    }
    let description = args
        .get("dish_description")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 500)
        .ok_or_else(|| {
            "dish_description must be a non-empty string of at most 500 characters".to_string()
        })?;
    let (path, body, draft_id) = match (draft, recipe) {
        (Some(id), None) => {
            if id.is_empty() {
                return Err("target ID must not be empty".into());
            }
            let revision = args
                .get("expected_revision")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .ok_or_else(|| "draft target requires a positive expected_revision".to_string())?;
            if fields.contains_key("base_revision") || fields.contains_key("replace") {
                return Err("draft and published recipe targets are mutually exclusive".into());
            }
            (
                format!("/api/v1/drafts/{}/cover", urlencoding(id)),
                json!({"expected_revision":revision,"dish_description":description}),
                Some(id),
            )
        }
        (None, Some(id)) => {
            if id.is_empty() {
                return Err("target ID must not be empty".into());
            }
            if fields.contains_key("expected_revision") {
                return Err("draft and published recipe targets are mutually exclusive".into());
            }
            if fields.contains_key("draft_id") {
                return Err("draft and published recipe targets are mutually exclusive".into());
            }
            let revision = args
                .get("base_revision")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "published recipe target requires base_revision".to_string())?;
            let replace = match args.get("replace") {
                None => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => return Err("replace must be a boolean".into()),
            };
            (
                format!("/api/v1/recipes/{}/cover", urlencoding(id)),
                json!({"base_revision":revision,"dish_description":description,"replace":replace}),
                None,
            )
        }
        _ => return Err("provide exactly one target: draft_id or recipe_id".into()),
    };
    let response = api.request(Method::POST, &path, Some(body)).await?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "could not read API response".to_string())?;
    if !status.is_success() {
        return Err(format!(
            "HTTP {}: cover generation request failed",
            status.as_u16()
        ));
    }
    let result: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid cover API response".to_string())?;
    let mut content = vec![json!({"type":"text","text":result.to_string()})];
    if let Some(id) = draft_id {
        let image_ref = result
            .get("image_ref")
            .and_then(Value::as_str)
            .ok_or("cover response missing image_ref")?;
        let expected = format!(
            "/api/v1/drafts/{}/covers/{}",
            urlencoding(id),
            urlencoding(image_ref)
        );
        if result.get("preview_url").and_then(Value::as_str) != Some(expected.as_str()) {
            return Err("cover preview path did not match the expected owner-private route".into());
        }
        let mut preview = api.request(Method::GET, &expected, None).await?;
        if !preview.status().is_success() {
            return Err("could not retrieve authenticated cover preview".into());
        }
        if preview
            .content_length()
            .is_some_and(|length| length > 5 * 1024 * 1024)
        {
            return Err("cover preview exceeds the 5 MiB MCP limit".into());
        }
        let mime = preview
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.starts_with("image/"))
            .ok_or("cover preview was not an image")?
            .to_owned();
        let mut image = Vec::new();
        while let Some(chunk) = preview
            .chunk()
            .await
            .map_err(|_| "could not read cover preview".to_string())?
        {
            if image.len() + chunk.len() > 5 * 1024 * 1024 {
                return Err("cover preview exceeds the 5 MiB MCP limit".into());
            }
            image.extend_from_slice(&chunk);
        }
        if image.is_empty() {
            return Err("cover preview is empty or exceeds the 5 MiB MCP limit".into());
        }
        content.push(json!({"type":"image","data":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, image),"mimeType":mime}));
    }
    Ok(content)
}

fn search_query_string(value: Option<&Value>) -> Result<String, String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let Value::Object(filters) = value else {
        return Err("query must be an object of named search filters".into());
    };
    let string_fields = [
        "q",
        "ingredients",
        "exclude_ingredients",
        "tags",
        "exclude_tags",
        "updated_since",
    ];
    let enum_fields = [
        ("ingredient_mode", &["all", "any"][..]),
        ("tag_mode", &["all", "any"][..]),
        ("sort", &["relevance", "title", "updated"][..]),
    ];
    let integer_fields = ["min_servings", "max_servings", "limit", "offset"];
    let boolean_fields = ["has_image", "include_deleted"];
    let mut pairs = Vec::with_capacity(filters.len());
    for (key, value) in filters {
        let encoded = if string_fields.contains(&key.as_str()) {
            value
                .as_str()
                .ok_or_else(|| format!("search filter {key} must be a string"))?
                .to_owned()
        } else if let Some((_, choices)) = enum_fields.iter().find(|(field, _)| field == key) {
            let text = value
                .as_str()
                .ok_or_else(|| format!("search filter {key} must be a string"))?;
            if !choices.contains(&text) {
                return Err(format!(
                    "search filter {key} must be one of {}",
                    choices.join(", ")
                ));
            }
            text.to_owned()
        } else if integer_fields.contains(&key.as_str()) {
            value
                .as_i64()
                .ok_or_else(|| format!("search filter {key} must be an integer"))?
                .to_string()
        } else if boolean_fields.contains(&key.as_str()) {
            value
                .as_bool()
                .ok_or_else(|| format!("search filter {key} must be a boolean"))?
                .to_string()
        } else {
            return Err(format!("unsupported search filter: {key}"));
        };
        pairs.push(format!("{}={}", urlencoding(key), urlencoding(&encoded)));
    }
    if pairs.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("?{}", pairs.join("&")))
    }
}

const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

fn read_local_image(path: PathBuf) -> Result<(Vec<u8>, &'static str), String> {
    let path_metadata = std::fs::metadata(&path)
        .map_err(|error| format!("cannot access image file {}: {error}", path.display()))?;
    if !path_metadata.is_file() {
        return Err(format!(
            "image path is not a regular file: {}",
            path.display()
        ));
    }
    let mime = match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        _ => {
            return Err(format!(
                "image file {} must have a .jpg, .jpeg, .png, or .webp extension",
                path.display()
            ))
        }
    };
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file: File = options
        .open(&path)
        .map_err(|error| format!("cannot open image file {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("cannot inspect image file {}: {error}", path.display()))?
        .is_file()
    {
        return Err(format!(
            "image path is not a regular file: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read image file {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("image exceeds API 10 MiB limit".into());
    }
    Ok((bytes, mime))
}
fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
async fn api_text(response: Response) -> Result<String, String> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "could not read API response".to_string())?;
    if !status.is_success() {
        let payload: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"message":String::from_utf8_lossy(&bytes)}));
        return Err(format!("HTTP {}: {}", status.as_u16(), payload));
    }
    if content_type.starts_with("text/") {
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    if content_type.starts_with("application/pdf") {
        return Ok(format!("PDF returned ({} bytes)", bytes.len()));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn handle(api: &Api, message: Value) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message["method"].as_str().unwrap_or("");
    let params = &message["params"];
    if message.get("id").is_none() && method.starts_with("notifications/") {
        return Value::Null;
    }
    let result = match method {
        "initialize" => Some(
            json!({"protocolVersion":negotiate_protocol(params.get("protocolVersion")),
            "capabilities":{"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false}},
            "instructions":"Recipe authoring workflow: call get_guidelines first, save or revise an owner-private draft, read it back with get_draft, validate the candidate, and only then explicitly call publish_draft. By default make one generate_cover attempt for a new dish unless the user opts out; inspect the private preview before explicitly publishing. If generation fails, report it and publish without a cover when requested. Drafts persist across sessions. Do not use HTTP recipe create/update routes for agent authoring.",
            "serverInfo":{"name":"recipe-mcp","version":"0.1.0"}}),
        ),
        "ping" => Some(json!({})),
        "tools/list" => Some(json!({"tools":tools()})),
        "resources/list" => Some(
            json!({"resources":[{"uri":"recipe://guidelines","name":"Recipe writing guidelines","mimeType":"text/markdown","description":"Required authoring guidance; read before creating or updating recipes."}]}),
        ),
        "resources/read" => match api.request(Method::GET, "/api/v1/guidelines", None).await {
            Ok(response) => match api_text(response).await {
                Ok(text) if params["uri"] == "recipe://guidelines" => Some(
                    json!({"contents":[{"uri":"recipe://guidelines","mimeType":"text/markdown","text":text}]}),
                ),
                Ok(_) => {
                    return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"unknown resource URI"}})
                }
                Err(e) => {
                    return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":e}})
                }
            },
            Err(e) => return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":e}}),
        },
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            if name == "generate_cover" {
                return match invoke_generate_cover(api, &params["arguments"]).await {
                    Ok(content) => json!({"jsonrpc":"2.0","id":id,"result":{"content":content}}),
                    Err(error) => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":error}],"isError":true}})
                    }
                };
            }
            match invoke(api, name, &params["arguments"]).await {
                Ok(text) => Some(json!({"content":[{"type":"text","text":text}]})),
                Err(error) => {
                    Some(json!({"content":[{"type":"text","text":error}],"isError":true}))
                }
            }
        }
        _ => {
            return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}})
        }
    };
    if message.get("id").is_none() {
        return Value::Null;
    }
    json!({"jsonrpc":"2.0","id":id,"result":result.unwrap_or(json!({}))})
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
#[tokio::main]
async fn main() {
    let api = match Api::new() {
        Ok(api) => api,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&api, message).await,
            Err(_) => {
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}})
            }
        };
        if !response.is_null() && writeln!(stdout, "{response}").is_err() {
            break;
        }
        let _ = stdout.flush();
    }
}
