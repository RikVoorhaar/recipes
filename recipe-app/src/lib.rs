pub mod config;
pub mod database;
mod pdf;
pub mod storage;

use axum::{
    body::Bytes,
    extract::{
        rejection::{BytesRejection, JsonRejection},
        Path, Query, State,
    },
    http::{header::AUTHORIZATION, HeaderMap, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use cooklang::CooklangParser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use std::{io, sync::Arc};
use storage::{FileImageStorage, ImageStorage};
use subtle::ConstantTimeEq;
use unicode_casefold::UnicodeCaseFold;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct RecipeService {
    pool: SqlitePool,
    parser: Arc<CooklangParser>,
    images: Arc<dyn ImageStorage>,
}

impl RecipeService {
    pub(crate) fn new(pool: SqlitePool, images: Arc<dyn ImageStorage>) -> Self {
        Self {
            pool,
            parser: Arc::new(CooklangParser::extended()),
            images,
        }
    }

    pub(crate) fn parse_body(&self, body: &str) -> Result<Vec<String>, ApiError> {
        validate_body(&self.parser, body)
    }
}

#[derive(Debug, Clone)]
pub struct ApiToken {
    pub label: String,
    pub secret: String,
}

impl ApiToken {
    pub fn from_config(value: &str) -> Result<Self, &'static str> {
        let (label, secret) = value.split_once(':').ok_or("token must be label:secret")?;
        if label.trim().is_empty() || secret.is_empty() {
            return Err("token must have a non-empty label and secret");
        }
        Ok(Self {
            label: label.trim().to_owned(),
            secret: secret.to_owned(),
        })
    }
}

pub fn app(pool: SqlitePool) -> Router {
    app_with_storage(pool, Arc::new(FileImageStorage::new("data/content")))
}

pub fn app_with_storage(pool: SqlitePool, images: Arc<dyn ImageStorage>) -> Router {
    api_router(pool, images)
}

pub async fn configure_tokens(pool: &SqlitePool, tokens: &[String]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM api_tokens")
        .execute(&mut *tx)
        .await?;
    for configured in tokens {
        let token = ApiToken::from_config(configured)
            .map_err(|error| sqlx::Error::Protocol(error.to_owned()))?;
        sqlx::query("INSERT INTO api_tokens(token_hash, label) VALUES (?, ?)")
            .bind(token_hash(&token.secret))
            .bind(token.label)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

fn api_router(pool: SqlitePool, images: Arc<dyn ImageStorage>) -> Router {
    let state = RecipeService::new(pool, images);
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/openapi.yaml", get(openapi))
        .route("/api/v1/guidelines", get(guidelines))
        .route("/api/v1/recipes", get(list_recipes).post(create_recipe))
        .route("/api/v1/recipes/:id/scaled", get(scaled_recipe))
        .route("/api/v1/recipes/:id/pdf", get(api_recipe_pdf))
        .route("/api/v1/recipes/validate", post(validate_recipe))
        .route("/", get(public_index))
        .route("/tags/:tag", get(public_tag))
        .route("/recipes/:id", get(public_recipe))
        .route("/api/v1/search", get(search_recipes))
        .route("/api/v1/images", post(upload_image))
        .route("/images/:ref", get(get_image))
        .route(
            "/api/v1/recipes/:id/image",
            post(attach_image).delete(detach_image),
        )
        .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024))
        .route("/api/v1/tags", get(list_tags))
        .route("/api/v1/ingredients", get(list_ingredients))
        .route(
            "/api/v1/ingredients/:id",
            get(get_ingredient).put(rename_ingredient),
        )
        .route("/api/v1/ingredients/:id/aliases", post(add_alias))
        .route(
            "/api/v1/ingredients/:id/aliases/:alias",
            delete(remove_alias),
        )
        .route("/api/v1/ingredients/:id/merge", post(merge_ingredient))
        .route(
            "/api/v1/recipes/:id",
            get(get_recipe).put(update_recipe).delete(delete_recipe),
        )
        .route("/api/v1/recipes/:id/restore", post(restore_recipe))
        .route("/api/v1/recipes/:id/revisions", get(list_revisions))
        .route(
            "/api/v1/recipes/:id/revisions/:revision",
            get(get_revision).post(restore_revision),
        )
        .layer(middleware::from_fn(log_request))
        .with_state(state)
}

async fn log_request(request: axum::extract::Request, next: middleware::Next) -> Response {
    tracing::info!(method = %request.method(), path = %request.uri().path(), "request received");
    next.run(request).await
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[derive(Debug, Serialize)]
pub(crate) struct ApiError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_revision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Vec<Diagnostic>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Diagnostic {
    line: usize,
    column: usize,
    message: String,
}

impl ApiError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            current_revision_id: None,
            diagnostics: None,
            details: None,
        }
    }

    fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.code {
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "not_found" => StatusCode::NOT_FOUND,
            "conflict" => StatusCode::CONFLICT,
            "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_json" => StatusCode::BAD_REQUEST,
            "cooklang_invalid"
            | "cooklang_metadata_forbidden"
            | "invalid_request"
            | "invalid_image"
            | "print_does_not_fit" => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self)).into_response()
    }
}

impl From<JsonRejection> for ApiError {
    fn from(_: JsonRejection) -> Self {
        Self::new(
            "invalid_json",
            "The request body must be valid JSON matching the endpoint schema",
        )
    }
}

fn token_hash(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

async fn authorize(state: &RecipeService, headers: &HeaderMap) -> Result<String, ApiError> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::new("unauthorized", "A bearer token is required"))?;
    let candidate = token_hash(value);
    let rows = sqlx::query_as::<_, TokenRow>("SELECT token_hash, label FROM api_tokens")
        .fetch_all(&state.pool)
        .await
        .map_err(internal_error)?;
    let mut matched_label = None;
    for row in rows {
        if row.token_hash.as_bytes().ct_eq(candidate.as_bytes()).into() {
            matched_label = Some(row.label);
        }
    }
    matched_label.ok_or_else(|| ApiError::new("unauthorized", "The bearer token is invalid"))
}

#[derive(FromRow)]
struct TokenRow {
    token_hash: String,
    label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeDocument {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub revision_created_at: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub servings: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub body: String,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub image: Option<ImageReference>,
    #[serde(default)]
    pub author_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReference {
    pub r#ref: String,
    pub url: String,
}

#[derive(Deserialize)]
pub struct RecipeWrite {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub servings: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub body: String,
    #[serde(default)]
    pub base_revision: Option<String>,
}

#[derive(Serialize)]
struct SaveResult {
    recipe: RecipeDocument,
    warnings: Vec<String>,
}

#[derive(FromRow)]
struct RevisionRow {
    id: String,
    recipe_id: String,
    created_at: String,
    author_label: String,
    title: String,
    description: String,
    servings: i64,
    tags: String,
    source_url: Option<String>,
    notes: Option<String>,
    body: String,
    deleted: i64,
    image_ref: Option<String>,
}

impl RevisionRow {
    fn document(self) -> RecipeDocument {
        RecipeDocument {
            id: Some(self.recipe_id),
            revision: Some(self.id),
            revision_created_at: Some(self.created_at),
            title: self.title,
            description: self.description,
            servings: self.servings as u32,
            tags: serde_json::from_str(&self.tags).unwrap_or_default(),
            source_url: self.source_url,
            notes: self.notes,
            body: self.body,
            deleted: self.deleted != 0,
            image: self.image_ref.map(|reference| ImageReference {
                url: format!("/images/{reference}"),
                r#ref: reference,
            }),
            author_label: Some(self.author_label),
        }
    }
}

const REVISION_COLUMNS: &str = "r.id, r.recipe_id, r.created_at, r.author_label, r.title, r.description, r.servings, r.tags, r.source_url, r.notes, r.body, r.deleted, r.image_ref";

async fn create_recipe(
    State(state): State<RecipeService>,
    headers: HeaderMap,
    request: Result<Json<RecipeWrite>, JsonRejection>,
) -> Result<(StatusCode, Json<SaveResult>), ApiError> {
    let author = authorize(&state, &headers).await?;
    let Json(write) = request?;
    let warnings = state.parse_body(&write.body)?;
    validate_write(&write)?;
    let id = new_id("rcp");
    let revision_id = new_id("rev");
    let created_at = now();
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    sqlx::query("INSERT INTO recipes(id, current_revision_id) VALUES (?, ?)")
        .bind(&id)
        .bind(&revision_id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    insert_revision(
        &mut tx,
        &revision_id,
        &id,
        &created_at,
        &author,
        &write,
        false,
        None,
    )
    .await?;
    derive_revision(&mut tx, &revision_id, &write.body, &author).await?;
    tx.commit().await.map_err(internal_error)?;
    let recipe = fetch_revision(&state.pool, &id, &revision_id)
        .await?
        .document();
    Ok((StatusCode::CREATED, Json(SaveResult { recipe, warnings })))
}

async fn update_recipe(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<RecipeWrite>, JsonRejection>,
) -> Result<Json<SaveResult>, ApiError> {
    let author = authorize(&state, &headers).await?;
    let Json(write) = request?;
    let base = write
        .base_revision
        .as_deref()
        .ok_or_else(|| ApiError::new("invalid_request", "base_revision is required for updates"))?;
    ensure_current_base(&state.pool, &id, base).await?;
    let current = fetch_revision(&state.pool, &id, base).await?;
    let image_ref = current.image_ref.clone();
    let revision_id = new_id("rev");
    let created_at = now();
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    let result = sqlx::query(
        "UPDATE recipes SET current_revision_id = ? WHERE id = ? AND current_revision_id = ? AND deleted = 0",
    )
    .bind(&revision_id)
    .bind(&id)
    .bind(base)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    if result.rows_affected() == 0 {
        tx.rollback().await.map_err(internal_error)?;
        return Err(conflict_error(
            &state.pool,
            &id,
            "The recipe changed, was deleted, or does not exist",
        )
        .await?);
    }
    if let Err(error) = validate_write(&write) {
        tx.rollback().await.map_err(internal_error)?;
        return Err(error);
    }
    let warnings = match state.parse_body(&write.body) {
        Ok(warnings) => warnings,
        Err(error) => {
            tx.rollback().await.map_err(internal_error)?;
            return Err(error);
        }
    };
    insert_revision(
        &mut tx,
        &revision_id,
        &id,
        &created_at,
        &author,
        &write,
        false,
        image_ref.as_deref(),
    )
    .await?;
    derive_revision(&mut tx, &revision_id, &write.body, &author).await?;
    tx.commit().await.map_err(internal_error)?;
    let recipe = fetch_revision(&state.pool, &id, &revision_id)
        .await?
        .document();
    Ok(Json(SaveResult { recipe, warnings }))
}

async fn delete_recipe(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<BaseRevision>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let author = authorize(&state, &headers).await?;
    let Json(request) = request?;
    append_state_revision(&state, &id, &request.base_revision, &author, true, None)
        .await
        .map(Json)
}

#[derive(Deserialize)]
struct BaseRevision {
    base_revision: String,
}

async fn restore_recipe(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<BaseRevision>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let author = authorize(&state, &headers).await?;
    let Json(request) = request?;
    append_state_revision(&state, &id, &request.base_revision, &author, false, None)
        .await
        .map(Json)
}

async fn restore_revision(
    State(state): State<RecipeService>,
    Path((id, revision)): Path<(String, String)>,
    headers: HeaderMap,
    request: Result<Json<BaseRevision>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let author = authorize(&state, &headers).await?;
    let Json(request) = request?;
    append_state_revision(
        &state,
        &id,
        &request.base_revision,
        &author,
        false,
        Some(revision),
    )
    .await
    .map(Json)
}

async fn append_state_revision(
    state: &RecipeService,
    id: &str,
    base: &str,
    author: &str,
    deleted: bool,
    restore: Option<String>,
) -> Result<RecipeDocument, ApiError> {
    let source = if let Some(revision) = restore {
        fetch_revision(&state.pool, id, &revision).await?
    } else {
        fetch_current(&state.pool, id).await?
    };
    let source_revision_id = source.id.clone();
    let source_image_ref = source.image_ref.clone();
    let revision_id = new_id("rev");
    let created_at = now();
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    let result = sqlx::query("UPDATE recipes SET current_revision_id = ?, deleted = ? WHERE id = ? AND current_revision_id = ?")
        .bind(&revision_id).bind(i64::from(deleted)).bind(id).bind(base)
        .execute(&mut *tx).await.map_err(internal_error)?;
    if result.rows_affected() == 0 {
        tx.rollback().await.map_err(internal_error)?;
        return Err(conflict_error(&state.pool, id, "The recipe changed or does not exist").await?);
    }
    let mut write = RecipeWrite {
        title: source.title,
        description: source.description,
        servings: source.servings as u32,
        tags: serde_json::from_str(&source.tags).unwrap_or_default(),
        source_url: source.source_url,
        notes: source.notes,
        body: source.body,
        base_revision: None,
    };
    insert_revision(
        &mut tx,
        &revision_id,
        id,
        &created_at,
        author,
        &write,
        deleted,
        source_image_ref.as_deref(),
    )
    .await?;
    copy_revision_derived(
        &mut tx,
        &source_revision_id,
        &revision_id,
        &write.body,
        author,
    )
    .await?;
    tx.commit().await.map_err(internal_error)?;
    write.base_revision = None;
    Ok(fetch_revision(&state.pool, id, &revision_id)
        .await?
        .document())
}

async fn list_recipes(
    State(state): State<RecipeService>,
    headers: HeaderMap,
) -> Result<Json<Vec<RecipeDocument>>, ApiError> {
    authorize(&state, &headers).await?;
    let query = format!("SELECT {REVISION_COLUMNS} FROM recipe_revisions r JOIN recipes p ON p.current_revision_id = r.id WHERE p.deleted = 0 ORDER BY r.title COLLATE NOCASE");
    let rows = sqlx::query_as::<_, RevisionRow>(&query)
        .fetch_all(&state.pool)
        .await
        .map_err(internal_error)?;
    Ok(Json(rows.into_iter().map(RevisionRow::document).collect()))
}

async fn get_recipe(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<RecipeDocument>, ApiError> {
    authorize(&state, &headers).await?;
    let row = fetch_current(&state.pool, &id).await?;
    Ok(Json(row.document()))
}

#[derive(Deserialize)]
struct ServingQuery {
    servings: Option<u32>,
}

#[derive(Clone, Serialize)]
struct ScaledIngredient {
    name: String,
    quantity: Option<String>,
    note: Option<String>,
}

#[derive(Clone, Serialize)]
struct ScaledRecipe {
    id: String,
    title: String,
    description: String,
    base_servings: u32,
    servings: u32,
    tags: Vec<String>,
    image: Option<ImageReference>,
    ingredients: Vec<ScaledIngredient>,
    instructions: Vec<String>,
    notes: Option<String>,
    source_url: Option<String>,
}

fn scaled_view(
    state: &RecipeService,
    row: &RevisionRow,
    target: u32,
) -> Result<ScaledRecipe, ApiError> {
    if target == 0 || target > 1000 {
        return Err(ApiError::new(
            "invalid_request",
            "servings must be between 1 and 1000",
        ));
    }
    let base = u32::try_from(row.servings)
        .map_err(|_| ApiError::new("invalid_request", "base servings are invalid"))?;
    let parsed =
        state.parser.parse(&row.body).into_output().ok_or_else(|| {
            ApiError::new("cooklang_invalid", "Cooklang body could not be parsed")
        })?;
    let mut recipe = parsed;
    if target != base {
        recipe.scale(target as f64 / base as f64, state.parser.converter());
    }
    let ingredients = recipe
        .ingredients
        .iter()
        .map(|ingredient| ScaledIngredient {
            name: ingredient.display_name().into_owned(),
            quantity: ingredient.quantity.as_ref().map(format_quantity),
            note: ingredient.note.clone(),
        })
        .collect();
    let instructions = recipe
        .sections
        .iter()
        .flat_map(|section| section.content.iter())
        .map(|content| match content {
            cooklang::Content::Step(step) => step
                .items
                .iter()
                .map(|item| match item {
                    cooklang::Item::Text { value } => value.clone(),
                    cooklang::Item::Ingredient { index } => {
                        recipe.ingredients[*index].display_name().into_owned()
                    }
                    cooklang::Item::Cookware { index } => recipe.cookware[*index].name.clone(),
                    cooklang::Item::Timer { index } => {
                        let timer = &recipe.timers[*index];
                        match (&timer.name, &timer.quantity) {
                            (Some(name), Some(quantity)) => {
                                format!("{name} ({quantity})")
                            }
                            (Some(name), None) => name.clone(),
                            (None, Some(quantity)) => quantity.to_string(),
                            (None, None) => String::new(),
                        }
                    }
                    cooklang::Item::InlineQuantity { index } => {
                        recipe.inline_quantities[*index].to_string()
                    }
                })
                .collect::<String>(),
            cooklang::Content::Text(value) => value.clone(),
        })
        .collect();
    Ok(ScaledRecipe {
        id: row.recipe_id.clone(),
        title: row.title.clone(),
        description: row.description.clone(),
        base_servings: base,
        servings: target,
        tags: serde_json::from_str(&row.tags).unwrap_or_default(),
        image: row.image_ref.clone().map(|reference| ImageReference {
            url: format!("/images/{reference}"),
            r#ref: reference,
        }),
        ingredients,
        instructions,
        notes: row.notes.clone(),
        source_url: row.source_url.clone(),
    })
}

fn format_quantity(quantity: &cooklang::quantity::Quantity) -> String {
    use cooklang::quantity::Value;

    let number = match quantity.value() {
        Value::Number(number) => format_quantity_number(number),
        Value::Range { start, end } => {
            format!(
                "{}-{}",
                format_quantity_number(start),
                format_quantity_number(end)
            )
        }
        Value::Text(_) => return quantity.to_string(),
    };
    match quantity.unit() {
        Some(unit) => format!("{number} {unit}"),
        None => number,
    }
}

fn format_quantity_number(number: &cooklang::quantity::Number) -> String {
    use cooklang::quantity::Number;

    let Number::Regular(value) = number else {
        return number.to_string();
    };
    let fraction = Number::new_approx(*value, 0.0001, 16, u32::MAX)
        .filter(|fraction| fraction.value() != 0.0 && (fraction.value() - value).abs() <= 0.0001);
    fraction.map_or_else(|| value.to_string(), |fraction| fraction.to_string())
}

async fn scaled_recipe(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ServingQuery>,
) -> Result<Json<ScaledRecipe>, ApiError> {
    authorize(&state, &headers).await?;
    let row = fetch_current(&state.pool, &id).await?;
    if row.deleted != 0 {
        return Err(ApiError::new("not_found", "Recipe not found"));
    }
    let target = query.servings.unwrap_or(row.servings as u32);
    Ok(Json(scaled_view(&state, &row, target)?))
}

async fn api_recipe_pdf(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ServingQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers).await?;
    let row = fetch_current(&state.pool, &id).await?;
    if row.deleted != 0 {
        return Err(ApiError::new("not_found", "Recipe not found"));
    }
    let target = query.servings.unwrap_or(row.servings as u32);
    render_pdf_response(&state, scaled_view(&state, &row, target)?).await
}

async fn render_pdf_response(
    state: &RecipeService,
    view: ScaledRecipe,
) -> Result<Response, ApiError> {
    let image = match &view.image {
        Some(image) => Some(state.images.get(&image.r#ref).map_err(internal_error)?),
        None => None,
    };
    let rendered = tokio::task::spawn_blocking(move || pdf::render(&view, image.as_deref()))
        .await
        .map_err(internal_error)?
        .map_err(render_error)?;
    let mut response = rendered.bytes.into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/pdf"),
    );
    response.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"recipe.pdf\""),
    );
    Ok(response)
}

fn render_error(error: pdf::RenderError) -> ApiError {
    match error {
        pdf::RenderError::DoesNotFit { pages } => ApiError::new(
            "print_does_not_fit",
            "Recipe does not fit on one A4 page at font sizes from 11 pt down to 8 pt",
        )
        .with_details(serde_json::json!({
            "page_count_at_minimum_font_size": pages,
            "minimum_font_size_pt": 8,
            "maximum_font_size_pt": 11
        })),
        pdf::RenderError::Compile(message) => internal_error(message),
    }
}

#[derive(Deserialize)]
struct ValidationRequest {
    title: String,
    servings: u32,
    body: String,
    target_servings: Option<u32>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    image_ref: Option<String>,
}

async fn validate_recipe(
    State(state): State<RecipeService>,
    headers: HeaderMap,
    request: Result<Json<ValidationRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers).await?;
    let Json(request) = request?;
    let warnings = state.parse_body(&request.body)?;
    let write = RecipeWrite {
        title: request.title.clone(),
        description: request.description.clone(),
        servings: request.servings,
        tags: request.tags.clone(),
        source_url: request.source_url.clone(),
        notes: request.notes.clone(),
        body: request.body.clone(),
        base_revision: None,
    };
    validate_write(&write)?;
    if let Some(image_ref) = request.image_ref.as_deref() {
        if !state.images.exists(image_ref).map_err(internal_error)? {
            return Err(ApiError::new("invalid_request", "image_ref does not exist"));
        }
    }
    let target = request.target_servings.unwrap_or(request.servings);
    if target == 0 || target > 1000 {
        return Err(ApiError::new(
            "invalid_request",
            "target_servings must be between 1 and 1000",
        ));
    }
    let ingredients = ingredient_names(&request.body);
    let mut resolutions = Vec::new();
    for name in &ingredients {
        let normalized = normalize(name);
        let found = resolve_ingredient_id(&state.pool, &normalized).await?;
        resolutions.push(serde_json::json!({
            "wording": name,
            "ingredient_id": found,
            "would_create": found.is_none(),
            "normalized_name": normalized
        }));
    }
    let row = RevisionRow {
        id: "dry-run".to_owned(),
        recipe_id: "dry-run".to_owned(),
        created_at: String::new(),
        author_label: String::new(),
        title: request.title,
        description: request.description,
        servings: request.servings as i64,
        tags: serde_json::to_string(&request.tags).map_err(internal_error)?,
        source_url: request.source_url,
        notes: request.notes,
        body: request.body,
        deleted: 0,
        image_ref: request.image_ref,
    };
    let view = scaled_view(&state, &row, target)?;
    let parsed = view
        .ingredients
        .iter()
        .map(|item| {
            serde_json::json!({
                "name": item.name,
                "quantity": item.quantity,
                "note": item.note
            })
        })
        .collect::<Vec<_>>();
    let fit_view = view.clone();
    let fit_image = fit_view
        .image
        .as_ref()
        .map(|image| state.images.get(&image.r#ref).map_err(internal_error))
        .transpose()?;
    let fit = tokio::task::spawn_blocking(move || pdf::render(&fit_view, fit_image.as_deref()))
        .await
        .map_err(internal_error)?;
    let (fits, font_size_pt, status) = match fit {
        Ok(rendered) => (true, Some(rendered.font_size_pt), "fits"),
        Err(pdf::RenderError::DoesNotFit { .. }) => (false, None, "does_not_fit"),
        Err(pdf::RenderError::Compile(message)) => return Err(internal_error(message)),
    };
    Ok(Json(serde_json::json!({
        "ingredients": parsed,
        "resolutions": resolutions,
        "warnings": warnings,
        "print_fit": {
            "fits": fits,
            "font_size_pt": font_size_pt,
            "status": status,
            "minimum_font_size_pt": 8
        }
    })))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn query_component(value: &str) -> String {
    value.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
        out
    })
}

fn safe_source_url(value: &str) -> Option<&str> {
    let authority = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))?
        .split(['/', '?', '#'])
        .next()?;
    (!authority.is_empty()
        && !authority.starts_with('.')
        && !authority.ends_with('.')
        && !value.chars().any(char::is_whitespace))
    .then_some(value)
}

async fn public_index(
    State(state): State<RecipeService>,
    Query(query): Query<PublicFilter>,
) -> Result<impl IntoResponse, ApiError> {
    let rows = public_rows(&state).await?;
    let filter = query.q.as_deref().unwrap_or("").to_lowercase();
    let tag = query.tag.as_deref().map(normalize);
    let cards = rows
        .into_iter()
        .filter(|row| {
            row.title.to_lowercase().contains(&filter)
                && tag.as_ref().is_none_or(|tag| {
                    serde_json::from_str::<Vec<String>>(&row.tags)
                        .unwrap_or_default()
                        .iter()
                        .any(|item| normalize(item) == *tag)
                })
        })
        .map(|row| public_card(&row))
        .collect::<String>();
    Ok(html(format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Recipes</title><style>*,*::before,*::after{{box-sizing:border-box}}body{{margin:0;font:1rem/1.5 system-ui,sans-serif;color:#25251f}}main{{width:min(calc(100% - 2rem),72rem);margin:auto;overflow-wrap:anywhere}}.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,16rem),1fr));gap:1rem}}.card{{min-width:0;overflow:hidden;border:1px solid #ddd;border-radius:.75rem}}.card>a{{display:block;color:inherit;text-decoration:none}}.card img,.placeholder{{display:block;width:100%;aspect-ratio:4/3;object-fit:cover}}.placeholder{{display:grid;place-items:center;background:#eee}}.card h2,.card p{{margin:.75rem}}img{{max-width:100%;height:auto}}a{{overflow-wrap:anywhere}}</style><main><h1>Recipes</h1><form><label>Title <input name=\"q\" value=\"{}\"></label><label>Tag <input name=\"tag\" value=\"{}\"></label><button>Filter</button></form><section class=\"grid\">{cards}</section></main></html>", escape_html(&filter), escape_html(query.tag.as_deref().unwrap_or("")))))
}

#[derive(Deserialize, Default)]
struct PublicFilter {
    q: Option<String>,
    tag: Option<String>,
}

async fn public_tag(
    State(state): State<RecipeService>,
    Path(tag): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    public_index(
        State(state),
        Query(PublicFilter {
            q: None,
            tag: Some(tag),
        }),
    )
    .await
}

async fn public_recipe(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    Query(query): Query<ServingQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (id, is_pdf) = id
        .strip_suffix(".pdf")
        .map_or((id.as_str(), false), |id| (id, true));
    let row = fetch_current(&state.pool, id).await?;
    if row.deleted != 0 {
        return Err(ApiError::new("not_found", "Recipe not found"));
    }
    let target = query.servings.unwrap_or(row.servings as u32);
    let view = scaled_view(&state, &row, target)?;
    if is_pdf {
        return render_pdf_response(&state, view).await;
    }
    let image = view
        .image
        .as_ref()
        .map(|image| {
            format!(
                "<img class=\"hero\" src=\"{}\" alt=\"\">",
                escape_html(&image.url)
            )
        })
        .unwrap_or_default();
    let ingredients = view
        .ingredients
        .iter()
        .map(|item| {
            format!(
                "<li>{} {}{}</li>",
                escape_html(item.quantity.as_deref().unwrap_or("")),
                escape_html(&item.name),
                item.note
                    .as_deref()
                    .filter(|note| !note.is_empty())
                    .map(|note| format!(" — {}", escape_html(note)))
                    .unwrap_or_default()
            )
        })
        .collect::<String>();
    let instructions = view
        .instructions
        .iter()
        .map(|step| format!("<li>{}</li>", escape_html(step)))
        .collect::<String>();
    let tags = view
        .tags
        .iter()
        .map(|tag| {
            format!(
                "<a href=\"/recipes?tag={}\">{}</a>",
                query_component(tag),
                escape_html(tag)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let source = view
        .source_url
        .as_deref()
        .and_then(safe_source_url)
        .map(|url| {
            format!(
                "<p><a rel=\"noopener noreferrer\" href=\"{}\">Source</a></p>",
                escape_html(url)
            )
        })
        .unwrap_or_default();
    let notes = view
        .notes
        .as_deref()
        .filter(|notes| !notes.is_empty())
        .map(|notes| {
            format!(
                "<section><h2>Notes</h2><p>{}</p></section>",
                escape_html(notes)
            )
        })
        .unwrap_or_default();
    Ok(html(format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title><style>*,*::before,*::after{{box-sizing:border-box}}body{{margin:0;font:1rem/1.5 system-ui,sans-serif;color:#25251f}}main{{width:min(calc(100% - 2rem),54rem);margin:auto;overflow-wrap:anywhere}}img{{display:block;max-width:100%;height:auto}}.hero{{width:100%;max-height:60vh;object-fit:cover}}a{{overflow-wrap:anywhere}}</style><main>{image}<h1>{}</h1><p>{}</p><p>{tags}</p><form><label>Servings <input type=\"number\" name=\"servings\" min=\"1\" max=\"1000\" value=\"{}\"></label><button>Scale</button></form><h2>Ingredients</h2><ul>{ingredients}</ul><h2>Method</h2><ol>{instructions}</ol>{notes}{source}<a href=\"/recipes/{}.pdf?servings={}\">Download PDF</a></main></html>", escape_html(&view.title), escape_html(&view.title), escape_html(&view.description), target, id, target)))
}

fn public_card(row: &RevisionRow) -> String {
    let image = row
        .image_ref
        .as_ref()
        .map(|reference| format!("<img src=\"/images/{}\" alt=\"\">", escape_html(reference)))
        .unwrap_or_else(|| {
            "<div class=\"placeholder\" role=\"img\" aria-label=\"No picture\">No picture</div>"
                .to_owned()
        });
    let tags = serde_json::from_str::<Vec<String>>(&row.tags)
        .unwrap_or_default()
        .iter()
        .map(|tag| {
            format!(
                "<a href=\"/recipes?tag={}\">{}</a>",
                query_component(tag),
                escape_html(tag)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<article class=\"card\"><a href=\"/recipes/{}\">{image}<h2>{}</h2></a><p>{tags}</p></article>",
        escape_html(&row.recipe_id),
        escape_html(&row.title)
    )
}

fn html(body: String) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn public_rows(state: &RecipeService) -> Result<Vec<RevisionRow>, ApiError> {
    sqlx::query_as::<_, RevisionRow>(&format!("SELECT {REVISION_COLUMNS} FROM recipe_revisions r JOIN recipes p ON p.current_revision_id=r.id WHERE p.deleted=0 ORDER BY r.title COLLATE NOCASE"))
        .fetch_all(&state.pool).await.map_err(internal_error)
}

async fn list_revisions(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<RecipeDocument>>, ApiError> {
    authorize(&state, &headers).await?;
    let query = format!("SELECT {REVISION_COLUMNS} FROM recipe_revisions r WHERE r.recipe_id = ? ORDER BY r.created_at, r.id");
    let rows = sqlx::query_as::<_, RevisionRow>(&query)
        .bind(&id)
        .fetch_all(&state.pool)
        .await
        .map_err(internal_error)?;
    if rows.is_empty() {
        return Err(ApiError::new("not_found", "Recipe not found"));
    }
    Ok(Json(rows.into_iter().map(RevisionRow::document).collect()))
}

async fn get_revision(
    State(state): State<RecipeService>,
    Path((id, revision)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<RecipeDocument>, ApiError> {
    authorize(&state, &headers).await?;
    Ok(Json(
        fetch_revision(&state.pool, &id, &revision)
            .await?
            .document(),
    ))
}

async fn guidelines(
    State(state): State<RecipeService>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    authorize(&state, &headers).await?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )],
        include_str!("../guidelines/recipe-writing.md"),
    ))
}

async fn openapi() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/yaml")],
        include_str!("../../openapi.yaml"),
    )
}

#[derive(Serialize)]
struct UploadedImage {
    r#ref: String,
    url: String,
}

async fn upload_image(
    State(state): State<RecipeService>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<UploadedImage>), ApiError> {
    authorize(&state, &headers).await?;
    let body = body.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::new("payload_too_large", "Image upload exceeds the 10 MiB limit")
        } else {
            ApiError::new("invalid_image", "Image request body could not be read")
        }
    })?;
    const MAX_UPLOAD: usize = 10 * 1024 * 1024;
    if body.is_empty() || body.len() > MAX_UPLOAD {
        return Err(ApiError::new(
            "invalid_image",
            "Image must be non-empty and no larger than 10 MiB",
        ));
    }
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !matches!(
        content_type.split(';').next().unwrap_or("").trim(),
        "image/jpeg" | "image/png" | "image/webp"
    ) {
        return Err(ApiError::new(
            "invalid_image",
            "Accepted image types are JPEG, PNG, and WebP",
        ));
    }
    let reader = image::ImageReader::new(io::Cursor::new(&body))
        .with_guessed_format()
        .map_err(|_| ApiError::new("invalid_image", "Image format could not be read"))?;
    let decoded_format = reader
        .format()
        .ok_or_else(|| ApiError::new("invalid_image", "Image format could not be identified"))?;
    let declared_format = match content_type.split(';').next().unwrap_or("").trim() {
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/png" => image::ImageFormat::Png,
        "image/webp" => image::ImageFormat::WebP,
        _ => unreachable!("content type validated above"),
    };
    if decoded_format != declared_format {
        return Err(ApiError::new(
            "invalid_image",
            "Declared content type does not match the decoded image format",
        ));
    }
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8_192);
    limits.max_image_height = Some(8_192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| ApiError::new("invalid_image", "Image is invalid or cannot be decoded"))?;
    if decoded.width() as u64 * decoded.height() as u64 > 16_000_000 {
        return Err(ApiError::new(
            "invalid_image",
            "Image dimensions may not exceed 16 megapixels",
        ));
    }
    let normalized = decoded.thumbnail(1600, 1600);
    let mut encoded = io::Cursor::new(Vec::new());
    normalized
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .map_err(internal_error)?;
    let reference = new_id("img");
    state
        .images
        .put(&reference, encoded.get_ref())
        .map_err(internal_error)?;
    Ok((
        StatusCode::CREATED,
        Json(UploadedImage {
            url: format!("/images/{reference}"),
            r#ref: reference,
        }),
    ))
}

async fn get_image(
    State(state): State<RecipeService>,
    Path(reference): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // Only refs actually issued by this application are accepted. The storage
    // backend validates opaque keys too, and never receives a client path.
    if !reference.starts_with("img_") {
        return Err(ApiError::new("not_found", "Image not found"));
    }
    let bytes = state.images.get(&reference).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ApiError::new("not_found", "Image not found")
        } else {
            internal_error(error)
        }
    })?;
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("image/jpeg"),
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    Ok(response)
}

#[derive(Deserialize)]
struct AttachImage {
    base_revision: String,
    image_ref: String,
}

async fn attach_image(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<AttachImage>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let author = authorize(&state, &headers).await?;
    let Json(request) = request?;
    if !request.image_ref.starts_with("img_")
        || !state
            .images
            .exists(&request.image_ref)
            .map_err(internal_error)?
    {
        return Err(ApiError::new("not_found", "Image not found"));
    }
    append_image_revision(
        &state,
        &id,
        &request.base_revision,
        Some(&request.image_ref),
        &author,
    )
    .await
    .map(Json)
}

async fn detach_image(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<BaseRevision>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let author = authorize(&state, &headers).await?;
    let Json(request) = request?;
    append_image_revision(&state, &id, &request.base_revision, None, &author)
        .await
        .map(Json)
}

async fn append_image_revision(
    state: &RecipeService,
    id: &str,
    base: &str,
    image_ref: Option<&str>,
    author: &str,
) -> Result<RecipeDocument, ApiError> {
    ensure_current_base(&state.pool, id, base).await?;
    let source = fetch_revision(&state.pool, id, base).await?;
    let revision_id = new_id("rev");
    let created_at = now();
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    let updated = sqlx::query("UPDATE recipes SET current_revision_id=? WHERE id=? AND current_revision_id=? AND deleted=0")
        .bind(&revision_id).bind(id).bind(base).execute(&mut *tx).await.map_err(internal_error)?;
    if updated.rows_affected() == 0 {
        tx.rollback().await.map_err(internal_error)?;
        return Err(conflict_error(
            &state.pool,
            id,
            "The recipe changed, was deleted, or does not exist",
        )
        .await?);
    }
    let write = RecipeWrite {
        title: source.title.clone(),
        description: source.description.clone(),
        servings: source.servings as u32,
        tags: serde_json::from_str(&source.tags).unwrap_or_default(),
        source_url: source.source_url.clone(),
        notes: source.notes.clone(),
        body: source.body.clone(),
        base_revision: None,
    };
    insert_revision(
        &mut tx,
        &revision_id,
        id,
        &created_at,
        author,
        &write,
        false,
        image_ref,
    )
    .await?;
    copy_revision_derived(&mut tx, base, &revision_id, &write.body, author).await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(fetch_revision(&state.pool, id, &revision_id)
        .await?
        .document())
}

#[derive(Serialize, FromRow)]
struct IngredientSummary {
    id: String,
    name: String,
    usage_count: i64,
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .case_fold()
        .collect()
}

fn ingredient_names(body: &str) -> Vec<String> {
    CooklangParser::extended()
        .parse(body)
        .into_result()
        .map(|(recipe, _)| {
            recipe
                .ingredients
                .into_iter()
                .map(|item| item.name)
                .collect()
        })
        .unwrap_or_default()
}

async fn canonical_id(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    wording: &str,
    author: &str,
) -> Result<String, ApiError> {
    let normalized = normalize(wording);
    if let Some(id) = sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(redirect_to, id) FROM ingredients WHERE normalized_name = ?",
    )
    .bind(&normalized)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_error)?
    {
        return Ok(id);
    }
    if let Some(id) = sqlx::query_scalar::<_, String>(
        "SELECT id FROM ingredients WHERE normalized_name = ? AND deleted = 0",
    )
    .bind(&normalized)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_error)?
    {
        return Ok(id);
    }
    if let Some(id) = sqlx::query_scalar::<_, String>(
        "SELECT ingredient_id FROM ingredient_aliases WHERE normalized_alias = ?",
    )
    .bind(&normalized)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_error)?
    {
        return Ok(id);
    }
    let id = new_id("ing");
    sqlx::query("INSERT INTO ingredients(id, name, normalized_name) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(wording.trim())
        .bind(&normalized)
        .execute(&mut **tx)
        .await
        .map_err(internal_error)?;
    history(tx, &id, "created", wording.trim(), "{}", author).await?;
    Ok(id)
}

async fn derive_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    revision: &str,
    body: &str,
    author: &str,
) -> Result<(), ApiError> {
    for (ordinal, wording) in ingredient_names(body).iter().enumerate() {
        let id = canonical_id(tx, wording, author).await?;
        let normalized = normalize(wording);
        sqlx::query("INSERT INTO ingredient_resolution_provenance(revision_id, ordinal, normalized_wording, ingredient_id) VALUES (?, ?, ?, ?)")
            .bind(revision).bind(ordinal as i64).bind(&normalized).bind(&id)
            .execute(&mut **tx).await.map_err(internal_error)?;
        sqlx::query("INSERT INTO ingredient_occurrences(revision_id, ordinal, wording, normalized_wording, ingredient_id) VALUES (?, ?, ?, ?, ?)")
            .bind(revision).bind(ordinal as i64).bind(wording).bind(normalized).bind(id)
            .execute(&mut **tx).await.map_err(internal_error)?;
    }
    Ok(())
}

/// Copies the resolved ingredient identity along with content-preserving
/// revisions (delete/restore). This also serves future metadata-only writes
/// such as attachment changes, which must not reinterpret the body under the
/// current alias registry.
async fn copy_revision_derived(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source_revision: &str,
    new_revision: &str,
    body: &str,
    author: &str,
) -> Result<(), ApiError> {
    let source_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ingredient_occurrences WHERE revision_id=?")
            .bind(source_revision)
            .fetch_one(&mut **tx)
            .await
            .map_err(internal_error)?;
    if source_count == 0 && !ingredient_names(body).is_empty() {
        // Compatibility fallback for a legacy revision not yet backfilled.
        return derive_revision(tx, new_revision, body, author).await;
    }

    sqlx::query(
        "INSERT INTO ingredient_resolution_provenance(revision_id,ordinal,normalized_wording,ingredient_id)
         SELECT ?, o.ordinal, o.normalized_wording, COALESCE(p.ingredient_id,o.ingredient_id)
         FROM ingredient_occurrences o
         LEFT JOIN ingredient_resolution_provenance p
           ON p.revision_id=o.revision_id AND p.ordinal=o.ordinal
         WHERE o.revision_id=?",
    )
    .bind(new_revision)
    .bind(source_revision)
    .execute(&mut **tx)
    .await
    .map_err(internal_error)?;
    sqlx::query(
        "INSERT INTO ingredient_occurrences(revision_id,ordinal,wording,normalized_wording,ingredient_id)
         SELECT ?, o.ordinal, o.wording, o.normalized_wording, COALESCE(p.ingredient_id,o.ingredient_id)
         FROM ingredient_occurrences o
         LEFT JOIN ingredient_resolution_provenance p
           ON p.revision_id=o.revision_id AND p.ordinal=o.ordinal
         WHERE o.revision_id=?",
    )
    .bind(new_revision)
    .bind(source_revision)
    .execute(&mut **tx)
    .await
    .map_err(internal_error)?;
    Ok(())
}

#[derive(Deserialize)]
struct IngredientQuery {
    q: Option<String>,
}

async fn list_ingredients(
    State(s): State<RecipeService>,
    headers: HeaderMap,
    Query(q): Query<IngredientQuery>,
) -> Result<Json<Vec<IngredientSummary>>, ApiError> {
    authorize(&s, &headers).await?;
    let query = q.q.as_deref().map(normalize);
    let rows = sqlx::query_as::<_, IngredientSummary>("SELECT i.id, i.name, COUNT(DISTINCT CASE WHEN p.deleted = 0 THEN p.id END) AS usage_count FROM ingredients i LEFT JOIN ingredient_occurrences o ON o.ingredient_id = COALESCE(i.redirect_to, i.id) LEFT JOIN recipe_revisions r ON r.id = o.revision_id LEFT JOIN recipes p ON p.current_revision_id = r.id WHERE i.deleted = 0 AND (? IS NULL OR i.normalized_name LIKE '%' || ? || '%') GROUP BY i.id ORDER BY i.name COLLATE NOCASE")
        .bind(&query).bind(&query).fetch_all(&s.pool).await.map_err(internal_error)?;
    Ok(Json(rows))
}

#[derive(Serialize, FromRow)]
struct IngredientDetail {
    id: String,
    name: String,
    usage_count: i64,
}

async fn get_ingredient(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&s, &headers).await?;
    let row = sqlx::query_as::<_, IngredientDetail>("SELECT i.id, i.name, COUNT(DISTINCT p.id) AS usage_count FROM ingredients i LEFT JOIN ingredient_occurrences o ON o.ingredient_id = COALESCE(i.redirect_to, i.id) LEFT JOIN recipe_revisions r ON r.id = o.revision_id LEFT JOIN recipes p ON p.current_revision_id = r.id AND p.deleted = 0 WHERE i.id = ? AND i.deleted = 0 GROUP BY i.id")
        .bind(&id).fetch_optional(&s.pool).await.map_err(internal_error)?.ok_or_else(|| ApiError::new("not_found", "Ingredient not found"))?;
    let aliases: Vec<String> = sqlx::query_scalar("SELECT alias FROM ingredient_aliases WHERE ingredient_id = ? ORDER BY alias COLLATE NOCASE").bind(&id).fetch_all(&s.pool).await.map_err(internal_error)?;
    let recipes: Vec<String> = sqlx::query_scalar("SELECT DISTINCT p.id FROM ingredient_occurrences o JOIN recipe_revisions r ON r.id=o.revision_id JOIN recipes p ON p.current_revision_id=r.id WHERE o.ingredient_id=? AND p.deleted=0 ORDER BY p.id").bind(&id).fetch_all(&s.pool).await.map_err(internal_error)?;
    Ok(Json(
        serde_json::json!({"id":row.id,"name":row.name,"usage_count":row.usage_count,"aliases":aliases,"recipe_ids":recipes}),
    ))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}
#[derive(Deserialize)]
struct AliasBody {
    alias: String,
}
#[derive(Deserialize)]
struct MergeBody {
    survivor: String,
}

async fn rename_ingredient(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<NameBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let author = authorize(&s, &headers).await?;
    let Json(b) = request?;
    let name = b.name.trim();
    let normalized = normalize(name);
    if normalized.is_empty() {
        return Err(ApiError::new("invalid_request", "name must not be empty"));
    }
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let collision: Option<String>=sqlx::query_scalar("SELECT id FROM ingredients WHERE normalized_name=? AND id<>? UNION SELECT ingredient_id FROM ingredient_aliases WHERE normalized_alias=?").bind(&normalized).bind(&id).bind(&normalized).fetch_optional(&mut *tx).await.map_err(internal_error)?;
    if collision.is_some() {
        return Err(ApiError::new(
            "conflict",
            "ingredient name is already in use",
        ));
    }
    let old: Option<String> =
        sqlx::query_scalar("SELECT name FROM ingredients WHERE id=? AND deleted=0")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal_error)?;
    let old = old.ok_or_else(|| ApiError::new("not_found", "Ingredient not found"))?;
    sqlx::query("UPDATE ingredients SET name=?, normalized_name=? WHERE id=?")
        .bind(name)
        .bind(&normalized)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query("INSERT INTO ingredient_aliases(normalized_alias,alias,ingredient_id) VALUES(?,?,?) ON CONFLICT(normalized_alias) DO UPDATE SET alias=excluded.alias, ingredient_id=excluded.ingredient_id").bind(normalize(&old)).bind(&old).bind(&id).execute(&mut *tx).await.map_err(internal_error)?;
    history(&mut tx, &id, "rename", name, "{}", &author).await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_alias(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<AliasBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let author = authorize(&s, &headers).await?;
    let Json(b) = request?;
    let alias = b.alias.trim();
    let normalized = normalize(alias);
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let exists: Option<String> =
        sqlx::query_scalar("SELECT id FROM ingredients WHERE id=? AND deleted=0")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal_error)?;
    if exists.is_none() {
        return Err(ApiError::new("not_found", "Ingredient not found"));
    }
    let collision: Option<String>=sqlx::query_scalar("SELECT id FROM ingredients WHERE normalized_name=? UNION SELECT ingredient_id FROM ingredient_aliases WHERE normalized_alias=?").bind(&normalized).bind(&normalized).fetch_optional(&mut *tx).await.map_err(internal_error)?;
    if collision.is_some() {
        return Err(ApiError::new(
            "conflict",
            "alias collides with a canonical name or existing alias",
        ));
    }
    sqlx::query(
        "INSERT INTO ingredient_aliases(normalized_alias,alias,ingredient_id) VALUES(?,?,?)",
    )
    .bind(&normalized)
    .bind(alias)
    .bind(&id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    history(&mut tx, &id, "alias_added", alias, "{}", &author).await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(StatusCode::CREATED)
}

async fn remove_alias(
    State(s): State<RecipeService>,
    Path((id, alias)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let author = authorize(&s, &headers).await?;
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let result =
        sqlx::query("DELETE FROM ingredient_aliases WHERE normalized_alias=? AND ingredient_id=?")
            .bind(normalize(&alias))
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new("not_found", "Alias not found"));
    }
    history(&mut tx, &id, "alias_removed", &alias, "{}", &author).await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn merge_ingredient(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<MergeBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let author = authorize(&s, &headers).await?;
    let Json(b) = request?;
    let survivor = normalize(&b.survivor);
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let source: Option<String> =
        sqlx::query_scalar("SELECT name FROM ingredients WHERE id=? AND deleted=0")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal_error)?;
    let source = source.ok_or_else(|| ApiError::new("not_found", "Source ingredient not found"))?;
    let target: Option<String> = sqlx::query_scalar(
        "SELECT id FROM ingredients WHERE id=? AND deleted=0 UNION SELECT id FROM ingredients WHERE normalized_name=? AND deleted=0 UNION SELECT ingredient_id FROM ingredient_aliases WHERE normalized_alias=? LIMIT 1",
    )
    .bind(&b.survivor)
    .bind(&survivor)
    .bind(&survivor)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal_error)?;
    let target =
        target.ok_or_else(|| ApiError::new("not_found", "Survivor ingredient not found"))?;
    if target == id {
        return Err(ApiError::new(
            "invalid_request",
            "source and survivor resolve to the same ingredient",
        ));
    }
    // Keep redirect resolution one hop so old names and IDs survive arbitrary
    // subsequent merges without relying on aliases or recursive joins.
    sqlx::query("UPDATE ingredients SET redirect_to=? WHERE redirect_to=?")
        .bind(&target)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query("UPDATE ingredient_occurrences SET ingredient_id=? WHERE ingredient_id=?")
        .bind(&target)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query("UPDATE ingredient_aliases SET ingredient_id=? WHERE ingredient_id=?")
        .bind(&target)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query(
        "UPDATE ingredient_resolution_provenance SET ingredient_id=? WHERE ingredient_id=?",
    )
    .bind(&target)
    .bind(&id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    sqlx::query("UPDATE ingredients SET deleted=1, redirect_to=? WHERE id=?")
        .bind(&target)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    history(
        &mut tx,
        &id,
        "merged",
        &source,
        &serde_json::json!({"survivor":target}).to_string(),
        &author,
    )
    .await?;
    history(
        &mut tx,
        &target,
        "merge_received",
        &source,
        &serde_json::json!({"source":id}).to_string(),
        &author,
    )
    .await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn history(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    action: &str,
    name: &str,
    details: &str,
    author: &str,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO ingredient_history(ingredient_id,action,name,details,created_at,author_label) VALUES(?,?,?,?,?,?)").bind(id).bind(action).bind(name).bind(details).bind(now()).bind(author).execute(&mut **tx).await.map_err(internal_error)?;
    Ok(())
}

async fn list_tags(
    State(s): State<RecipeService>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    authorize(&s, &headers).await?;
    let rows:Vec<(String,i64)>=sqlx::query_as("SELECT j.value,COUNT(DISTINCT p.id) FROM recipes p JOIN recipe_revisions r ON r.id=p.current_revision_id JOIN json_each(r.tags) j WHERE p.deleted=0 GROUP BY j.value ORDER BY j.value COLLATE NOCASE").fetch_all(&s.pool).await.map_err(internal_error)?;
    Ok(Json(
        rows.into_iter()
            .map(|(tag, count)| serde_json::json!({"tag":tag,"count":count}))
            .collect(),
    ))
}

#[derive(Deserialize, Default)]
struct SearchQuery {
    q: Option<String>,
    ingredients: Option<String>,
    ingredient_mode: Option<String>,
    exclude_ingredients: Option<String>,
    tags: Option<String>,
    tag_mode: Option<String>,
    exclude_tags: Option<String>,
    min_servings: Option<u32>,
    max_servings: Option<u32>,
    has_image: Option<bool>,
    include_deleted: Option<bool>,
    updated_since: Option<String>,
    sort: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn search_recipes(
    State(s): State<RecipeService>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    authorize(&s, &headers).await?;
    let rows:Vec<RevisionRow>=sqlx::query_as::<_,RevisionRow>(&format!("SELECT {REVISION_COLUMNS} FROM recipes p JOIN recipe_revisions r ON r.id=p.current_revision_id ORDER BY r.title COLLATE NOCASE")).fetch_all(&s.pool).await.map_err(internal_error)?;
    let names = |v: &Option<String>| {
        v.as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(normalize)
            .collect::<Vec<_>>()
    };
    let requested = names(&q.ingredients);
    let excluded = names(&q.exclude_ingredients);
    let tags = names(&q.tags);
    let no_tags = names(&q.exclude_tags);
    let mut want = Vec::new();
    let mut unresolved_requested = false;
    for value in &requested {
        if let Some(id) = resolve_ingredient_id(&s.pool, value).await? {
            if !want.contains(&id) {
                want.push(id);
            }
        } else {
            unresolved_requested = true;
        }
    }
    let mut excluded_ids = Vec::new();
    for value in &excluded {
        if let Some(id) = resolve_ingredient_id(&s.pool, value).await? {
            if !excluded_ids.contains(&id) {
                excluded_ids.push(id);
            }
        }
    }
    let has_requested_ingredients = !requested.is_empty();
    let since = q
        .updated_since
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| ApiError::new("invalid_request", "updated_since must be RFC3339"))?;
    if q.ingredient_mode
        .as_deref()
        .is_some_and(|value| value != "all" && value != "any")
        || q.tag_mode
            .as_deref()
            .is_some_and(|value| value != "all" && value != "any")
        || q.sort
            .as_deref()
            .is_some_and(|value| value != "title" && value != "updated" && value != "relevance")
    {
        return Err(ApiError::new(
            "invalid_request",
            "search modes must be all/any and sort must be title/updated/relevance",
        ));
    }
    let mut found = Vec::new();
    for r in rows {
        if r.deleted != 0 && !q.include_deleted.unwrap_or(false) {
            continue;
        }
        if q.min_servings.is_some_and(|v| r.servings < v as i64)
            || q.max_servings.is_some_and(|v| r.servings > v as i64)
        {
            continue;
        }
        if let Some(since) = since {
            let Some(updated) = chrono::DateTime::parse_from_rfc3339(&r.created_at).ok() else {
                continue;
            };
            if updated < since {
                continue;
            }
        }
        if q.has_image
            .is_some_and(|has_image| r.image_ref.is_some() != has_image)
        {
            continue;
        }
        let body = r.body.to_lowercase();
        let title = r.title.to_lowercase();
        let description = r.description.to_lowercase();
        let notes = r.notes.clone().unwrap_or_default().to_lowercase();
        if q.q.as_ref().is_some_and(|v| {
            let needle = v.to_lowercase();
            ![&title, &description, &notes, &body]
                .iter()
                .any(|x| x.contains(&needle))
        }) {
            continue;
        }
        let doc = r.document();
        let recipe_tags: Vec<String> = doc.tags.iter().map(|x| normalize(x)).collect();
        if tags.iter().any(|t| !recipe_tags.contains(t))
            && q.tag_mode.as_deref().unwrap_or("all") == "all"
        {
            continue;
        }
        if !tags.is_empty()
            && q.tag_mode.as_deref() == Some("any")
            && !tags.iter().any(|t| recipe_tags.contains(t))
        {
            continue;
        }
        if no_tags.iter().any(|t| recipe_tags.contains(t)) {
            continue;
        }
        let revision = doc.revision.clone().unwrap_or_default();
        let linked: Vec<(String, String)> = sqlx::query_as(
            "SELECT ingredient_id, wording FROM ingredient_occurrences WHERE revision_id=? ORDER BY ordinal",
        )
        .bind(&revision)
        .fetch_all(&s.pool)
        .await
        .map_err(internal_error)?;
        if linked.iter().any(|(id, _)| excluded_ids.contains(id)) {
            continue;
        }
        if has_requested_ingredients
            && (want.is_empty()
                || (unresolved_requested && q.ingredient_mode.as_deref().unwrap_or("all") == "all"))
        {
            continue;
        }
        if !want.is_empty()
            && q.ingredient_mode.as_deref().unwrap_or("all") == "all"
            && want
                .iter()
                .any(|id| !linked.iter().any(|(item_id, _)| item_id == id))
        {
            continue;
        }
        if !want.is_empty()
            && q.ingredient_mode.as_deref() == Some("any")
            && !want
                .iter()
                .any(|id| linked.iter().any(|(item_id, _)| item_id == id))
        {
            continue;
        }
        if let Some(needle) = q.q.as_ref() {
            found.push((doc, linked, needle.to_lowercase()))
        } else {
            found.push((doc, linked, String::new()))
        }
    }
    match q.sort.as_deref().unwrap_or("title") {
        "updated" => found.sort_by(|a, b| {
            let updated = |value: &Option<String>| {
                value
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.timestamp_millis())
                    .unwrap_or_default()
            };
            updated(&b.0.revision_created_at)
                .cmp(&updated(&a.0.revision_created_at))
                .then_with(|| a.0.title.cmp(&b.0.title))
                .then_with(|| a.0.id.cmp(&b.0.id))
        }),
        "relevance" => {
            let n = q.q.as_deref().unwrap_or("").to_lowercase();
            found.sort_by(|a, b| {
                (!a.0.title.to_lowercase().contains(&n))
                    .cmp(&(!b.0.title.to_lowercase().contains(&n)))
                    .then_with(|| a.0.title.to_lowercase().cmp(&b.0.title.to_lowercase()))
                    .then_with(|| a.0.id.cmp(&b.0.id))
            })
        }
        _ => found.sort_by(|a, b| {
            a.0.title
                .to_lowercase()
                .cmp(&b.0.title.to_lowercase())
                .then_with(|| a.0.id.cmp(&b.0.id))
        }),
    }
    let offset = q.offset.unwrap_or(0) as usize;
    let limit = q.limit.unwrap_or(50).min(200) as usize;
    let result=found.into_iter().skip(offset).take(limit).map(|(doc,linked,needle)|{let matched:Vec<String>=linked.into_iter().filter(|(id,word)|if has_requested_ingredients { want.contains(id) } else { !needle.is_empty() && normalize(word).contains(&normalize(&needle)) }).map(|(_,word)|word).collect();let snippet=doc.body.chars().take(180).collect::<String>();serde_json::json!({"id":doc.id,"title":doc.title,"tags":doc.tags,"matched_ingredients":matched,"snippet":snippet})}).collect();
    Ok(Json(result))
}

async fn resolve_ingredient_id(pool: &SqlitePool, value: &str) -> Result<Option<String>, ApiError> {
    sqlx::query_scalar(
        "SELECT COALESCE(redirect_to,id) FROM ingredients WHERE id=? OR normalized_name=? UNION SELECT COALESCE(i.redirect_to,i.id) FROM ingredient_aliases a JOIN ingredients i ON i.id=a.ingredient_id WHERE a.normalized_alias=? LIMIT 1",
    )
    .bind(value)
    .bind(normalize(value))
    .bind(normalize(value))
    .fetch_optional(pool)
    .await
    .map_err(internal_error)
}

pub async fn rebuild_derived(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM ingredient_occurrences")
        .execute(&mut *tx)
        .await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id,body FROM recipe_revisions ORDER BY created_at,id")
            .fetch_all(&mut *tx)
            .await?;
    for (revision, body) in rows {
        for (ordinal, wording) in ingredient_names(&body).iter().enumerate() {
            let norm = normalize(wording);
            let id: Option<String> = sqlx::query_scalar(
                "SELECT ingredient_id FROM ingredient_resolution_provenance WHERE revision_id=? AND ordinal=? AND normalized_wording=?",
            )
            .bind(&revision)
            .bind(ordinal as i64)
            .bind(&norm)
            .fetch_optional(&mut *tx)
            .await?;
            let id = if let Some(id) = id {
                id
            } else if let Some(id) = sqlx::query_scalar(
                "SELECT COALESCE(redirect_to,id) FROM ingredients WHERE normalized_name=?",
            )
            .bind(&norm)
            .fetch_optional(&mut *tx)
            .await?
            {
                id
            } else if let Some(id) = sqlx::query_scalar(
                "SELECT ingredient_id FROM ingredient_aliases WHERE normalized_alias=?",
            )
            .bind(&norm)
            .fetch_optional(&mut *tx)
            .await?
            {
                id
            } else {
                let id = new_id("ing");
                sqlx::query("INSERT INTO ingredients(id,name,normalized_name) VALUES(?,?,?)")
                    .bind(&id)
                    .bind(wording.trim())
                    .bind(&norm)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO ingredient_history(ingredient_id,action,name,details,created_at,author_label) VALUES(?, 'created', ?, '{}', ?, 'derived-data-rebuild')")
                    .bind(&id)
                    .bind(wording.trim())
                    .bind(now())
                    .execute(&mut *tx)
                    .await?;
                id
            };
            sqlx::query("INSERT INTO ingredient_resolution_provenance(revision_id,ordinal,normalized_wording,ingredient_id) VALUES(?,?,?,?) ON CONFLICT(revision_id,ordinal) DO UPDATE SET normalized_wording=excluded.normalized_wording, ingredient_id=excluded.ingredient_id")
                .bind(&revision).bind(ordinal as i64).bind(&norm).bind(&id).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO ingredient_occurrences(revision_id,ordinal,wording,normalized_wording,ingredient_id) VALUES(?,?,?,?,?)").bind(&revision).bind(ordinal as i64).bind(wording).bind(&norm).bind(id).execute(&mut *tx).await?;
        }
    }
    sqlx::query("INSERT INTO app_metadata(key,value) VALUES('derived_ingredient_provenance_v1','ready') ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

pub async fn ensure_derived(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let initialized: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_metadata WHERE key='derived_ingredient_provenance_v1'",
    )
    .fetch_optional(pool)
    .await?;
    if initialized.as_deref() != Some("ready") {
        rebuild_derived(pool).await?;
    }
    Ok(())
}

fn validate_write(write: &RecipeWrite) -> Result<(), ApiError> {
    if write.title.trim().is_empty() || write.servings == 0 {
        return Err(ApiError::new(
            "invalid_request",
            "title and positive servings are required",
        ));
    }
    Ok(())
}

fn validate_body(parser: &CooklangParser, body: &str) -> Result<Vec<String>, ApiError> {
    let result = parser.parse(body);
    match result.into_result() {
        Ok((recipe, warnings)) => {
            if !recipe.metadata.map.is_empty() {
                return Err(ApiError::new(
                    "cooklang_metadata_forbidden",
                    "Cooklang metadata/front matter is forbidden; use structured API fields",
                ));
            }
            Ok(warnings.warnings().map(ToString::to_string).collect())
        }
        Err(report) => {
            let diagnostics = report
                .into_vec()
                .into_iter()
                .map(|diagnostic| {
                    let offset = diagnostic
                        .labels
                        .first()
                        .map(|label| label.0.start())
                        .unwrap_or(0);
                    let prefix = body.chars().take(offset).collect::<String>();
                    Diagnostic {
                        line: prefix.matches('\n').count() + 1,
                        column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
                        message: diagnostic.to_string(),
                    }
                })
                .collect();
            let mut api_error =
                ApiError::new("cooklang_invalid", "Cooklang body could not be parsed");
            api_error.diagnostics = Some(diagnostics);
            Err(api_error)
        }
    }
}

async fn fetch_current(pool: &SqlitePool, id: &str) -> Result<RevisionRow, ApiError> {
    let query = format!("SELECT {REVISION_COLUMNS} FROM recipes p JOIN recipe_revisions r ON r.id = p.current_revision_id WHERE p.id = ?");
    sqlx::query_as::<_, RevisionRow>(&query)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::new("not_found", "Recipe not found"))
}

async fn fetch_revision(
    pool: &SqlitePool,
    id: &str,
    revision: &str,
) -> Result<RevisionRow, ApiError> {
    let query = format!(
        "SELECT {REVISION_COLUMNS} FROM recipe_revisions r WHERE r.recipe_id = ? AND r.id = ?"
    );
    sqlx::query_as::<_, RevisionRow>(&query)
        .bind(id)
        .bind(revision)
        .fetch_optional(pool)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| ApiError::new("not_found", "Recipe revision not found"))
}

async fn conflict_error(pool: &SqlitePool, id: &str, message: &str) -> Result<ApiError, ApiError> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT current_revision_id FROM recipes WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(internal_error)?;
    let Some(current_revision_id) = current else {
        return Ok(ApiError::new("not_found", "Recipe not found"));
    };
    let mut error = ApiError::new("conflict", message);
    error.current_revision_id = Some(current_revision_id);
    Ok(error)
}

async fn ensure_current_base(pool: &SqlitePool, id: &str, base: &str) -> Result<(), ApiError> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT current_revision_id FROM recipes WHERE id=?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(internal_error)?;
    let Some(current) = current else {
        return Err(ApiError::new("not_found", "Recipe not found"));
    };
    if current != base {
        return Err(conflict_error(pool, id, "The recipe changed").await?);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    revision_id: &str,
    id: &str,
    created_at: &str,
    author: &str,
    write: &RecipeWrite,
    deleted: bool,
    image_ref: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO recipe_revisions(id, recipe_id, created_at, author_label, title, description, servings, tags, source_url, notes, body, deleted, image_ref) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(revision_id).bind(id).bind(created_at).bind(author).bind(&write.title)
        .bind(&write.description).bind(i64::from(write.servings))
        .bind(serde_json::to_string(&write.tags).map_err(internal_error)?)
        .bind(&write.source_url).bind(&write.notes).bind(&write.body).bind(i64::from(deleted)).bind(image_ref)
        .execute(&mut **tx).await.map_err(internal_error)?;
    Ok(())
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn internal_error(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %error, "API operation failed");
    ApiError::new("internal_error", "The request could not be completed")
}
