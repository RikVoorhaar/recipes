pub mod config;
pub mod cover;
pub mod database;
// The HTTP MCP transport in #10 will wire this module into the router.
#[allow(dead_code)]
mod mcp;
mod pdf;
mod recipe_web;
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
use cover::Generator;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use std::{
    collections::{HashMap, HashSet},
    io,
    sync::Arc,
};
use storage::{FileImageStorage, ImageStorage};
use subtle::ConstantTimeEq;
use unicode_casefold::UnicodeCaseFold;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct RecipeService {
    pool: SqlitePool,
    parser: Arc<CooklangParser>,
    images: Arc<dyn ImageStorage>,
    cover_generator: Option<Arc<Generator>>,
}

impl RecipeService {
    pub(crate) fn new(
        pool: SqlitePool,
        images: Arc<dyn ImageStorage>,
        cover_generator: Option<Arc<Generator>>,
    ) -> Self {
        Self {
            pool,
            parser: Arc::new(CooklangParser::extended()),
            images,
            cover_generator,
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
    app_with_storage_and_cover_generator(pool, images, None)
}

pub fn app_with_storage_and_cover_generator(
    pool: SqlitePool,
    images: Arc<dyn ImageStorage>,
    cover_generator: Option<Arc<Generator>>,
) -> Router {
    api_router(pool, images, cover_generator)
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

fn api_router(
    pool: SqlitePool,
    images: Arc<dyn ImageStorage>,
    cover_generator: Option<Arc<Generator>>,
) -> Router {
    let state = RecipeService::new(pool, images, cover_generator);
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/openapi.yaml", get(openapi))
        .route("/api/v1/guidelines", get(guidelines))
        .route("/api/v1/drafts", get(api_list_drafts).post(api_save_draft))
        .route(
            "/api/v1/drafts/:id",
            get(api_get_draft).delete(api_discard_draft),
        )
        .route("/api/v1/drafts/:id/publish", post(api_publish_draft))
        .route("/api/v1/drafts/:id/cover", post(api_generate_draft_cover))
        .route(
            "/api/v1/drafts/:id/covers/:image_ref",
            get(api_get_draft_cover),
        )
        .route("/api/v1/recipes", get(api_list_recipes).post(create_recipe))
        .route("/api/v1/recipes/:id/scaled", get(scaled_recipe))
        .route("/api/v1/recipes/:id/pdf", get(api_recipe_pdf))
        .route("/api/v1/recipes/validate", post(api_validate_recipe))
        .route("/", get(public_index))
        .route("/recipes", get(public_index))
        .route("/ingredients", get(public_ingredients))
        .route("/assets/site.css", get(public_stylesheet))
        .route("/assets/Lora.ttf", get(public_font))
        .route("/tags/:tag", get(public_tag))
        .route("/recipes/:id", get(public_recipe))
        .route("/api/v1/search", get(api_search_recipes))
        .route("/api/v1/images", post(upload_image))
        .route("/images/:ref", get(get_image))
        .route(
            "/api/v1/recipes/:id/image",
            post(api_attach_image).delete(detach_image),
        )
        .route("/api/v1/recipes/:id/cover", post(api_generate_recipe_cover))
        .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024))
        .route("/api/v1/tags", get(api_list_tags))
        .route("/api/v1/ingredients", get(api_list_ingredients))
        .route(
            "/api/v1/ingredients/:id",
            get(api_get_ingredient).put(api_rename_ingredient),
        )
        .route("/api/v1/ingredients/:id/aliases", post(api_add_alias))
        .route(
            "/api/v1/ingredients/:id/aliases/:alias",
            delete(api_remove_alias),
        )
        .route("/api/v1/ingredients/:id/merge", post(api_merge_ingredient))
        .route(
            "/api/v1/recipes/:id",
            get(api_get_recipe).put(update_recipe).delete(delete_recipe),
        )
        .route("/api/v1/recipes/:id/restore", post(restore_recipe))
        .route("/api/v1/recipes/:id/revisions", get(api_list_revisions))
        .route(
            "/api/v1/recipes/:id/revisions/:revision",
            get(api_get_revision).post(restore_revision),
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
    authorize_identity(state, headers)
        .await
        .map(|caller| caller.label)
}

struct CallerIdentity {
    label: String,
    token_hash: String,
}

async fn authorize_identity(
    state: &RecipeService,
    headers: &HeaderMap,
) -> Result<CallerIdentity, ApiError> {
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
    matched_label
        .map(|label| CallerIdentity {
            label,
            token_hash: candidate,
        })
        .ok_or_else(|| ApiError::new("unauthorized", "The bearer token is invalid"))
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

#[derive(Debug, Serialize, Deserialize)]
struct DraftDocument {
    id: String,
    revision: i64,
    base_recipe_id: Option<String>,
    base_revision: Option<String>,
    document: serde_json::Value,
    updated_at: String,
}

#[derive(FromRow)]
struct DraftRow {
    id: String,
    revision: i64,
    base_recipe_id: Option<String>,
    base_revision_id: Option<String>,
    document: String,
    updated_at: String,
}

impl DraftRow {
    fn document(self) -> Result<DraftDocument, ApiError> {
        Ok(DraftDocument {
            id: self.id,
            revision: self.revision,
            base_recipe_id: self.base_recipe_id,
            base_revision: self.base_revision_id,
            document: serde_json::from_str(&self.document).map_err(internal_error)?,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Deserialize)]
struct DraftWrite {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    expected_revision: Option<i64>,
    #[serde(default)]
    recipe_id: Option<String>,
    document: serde_json::Value,
}

#[derive(Deserialize)]
struct DraftRevision {
    expected_revision: i64,
    #[serde(default)]
    image_ref: Option<String>,
}

#[derive(Deserialize)]
struct ExpectedDraftRevision {
    expected_revision: i64,
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

async fn api_save_draft(
    State(state): State<RecipeService>,
    headers: HeaderMap,
    request: Result<Json<DraftWrite>, JsonRejection>,
) -> Result<(StatusCode, Json<DraftDocument>), ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(write) = request?;
    let draft = save_draft(&state, &caller, write).await?;
    let status = if draft.revision == 1 {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(draft)))
}

async fn save_draft(
    state: &RecipeService,
    caller: &CallerIdentity,
    mut write: DraftWrite,
) -> Result<DraftDocument, ApiError> {
    let owner_id = &caller.token_hash;
    if !write.document.is_object() {
        return Err(ApiError::new(
            "invalid_request",
            "document must be a JSON object",
        ));
    }
    let timestamp = now();
    let id = write.id.take().unwrap_or_else(|| new_id("drf"));
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    if let Some(expected) = write.expected_revision {
        let locked = sqlx::query(
            "UPDATE recipe_drafts SET revision=revision WHERE id=? AND owner_token_hash=? AND revision=?",
        )
        .bind(&id)
        .bind(owner_id)
        .bind(expected)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
        if locked.rows_affected() == 0 {
            return Err(ApiError::new(
                "conflict",
                "Draft not found or revision changed",
            ));
        }
    } else {
        // Lock a real existing row before reads. Updating a nonexistent draft ID
        // may not acquire SQLite's writer lock and leaves a deferred read txn.
        let locked = if let Some(recipe_id) = write.recipe_id.as_deref() {
            sqlx::query("UPDATE recipes SET current_revision_id=current_revision_id WHERE id=?")
                .bind(recipe_id)
                .execute(&mut *tx)
                .await
        } else {
            sqlx::query("UPDATE api_tokens SET label=label WHERE token_hash=?")
                .bind(owner_id)
                .execute(&mut *tx)
                .await
        }
        .map_err(internal_error)?;
        if locked.rows_affected() == 0 {
            return Err(ApiError::new("not_found", "Recipe not found"));
        }
    }
    let existing = sqlx::query_as::<_, DraftRow>(
        "SELECT id,revision,base_recipe_id,base_revision_id,document,updated_at FROM recipe_drafts WHERE id=? AND owner_token_hash=?",
    )
    .bind(&id).bind(owner_id).fetch_optional(&mut *tx).await.map_err(internal_error)?;
    let (revision, base_recipe, base_revision, document) = if let Some(existing) = existing {
        let expected = write.expected_revision.ok_or_else(|| {
            ApiError::new(
                "invalid_request",
                "expected_revision is required to revise a draft",
            )
        })?;
        if expected != existing.revision {
            tx.rollback().await.map_err(internal_error)?;
            return Err(ApiError::new("conflict", "The draft changed"));
        }
        if let Some(object) = write.document.as_object_mut() {
            let mut merged = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                &existing.document,
            )
            .map_err(internal_error)?;
            merged.append(object);
            write.document = serde_json::Value::Object(merged);
        }
        (
            existing.revision + 1,
            existing.base_recipe_id,
            existing.base_revision_id,
            write.document,
        )
    } else {
        if write.expected_revision.is_some() {
            tx.rollback().await.map_err(internal_error)?;
            return Err(ApiError::new("not_found", "Draft not found"));
        }
        let (base_recipe, base_revision) = if let Some(recipe_id) = write.recipe_id.as_deref() {
            let query = format!(
                "SELECT {REVISION_COLUMNS} FROM recipes p JOIN recipe_revisions r ON r.id=p.current_revision_id WHERE p.id=?"
            );
            let current = sqlx::query_as::<_, RevisionRow>(&query)
                .bind(recipe_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal_error)?
                .ok_or_else(|| ApiError::new("not_found", "Recipe not found"))?;
            let base_revision = current.id.clone();
            let mut base: serde_json::Value =
                serde_json::to_value(current.document()).map_err(internal_error)?;
            if let (Some(target), Some(fields)) = (base.as_object_mut(), write.document.as_object())
            {
                target.extend(fields.clone());
            }
            write.document = base;
            (Some(recipe_id.to_owned()), Some(base_revision))
        } else {
            (None, None)
        };
        (1, base_recipe, base_revision, write.document)
    };
    let encoded = serde_json::to_string(&document).map_err(internal_error)?;
    let saved = sqlx::query("INSERT INTO recipe_drafts(id,owner_token_hash,revision,base_recipe_id,base_revision_id,document,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision, document=excluded.document, updated_at=excluded.updated_at WHERE owner_token_hash=excluded.owner_token_hash AND revision=?")
        .bind(&id).bind(owner_id).bind(revision).bind(&base_recipe).bind(&base_revision).bind(encoded).bind(&timestamp).bind(&timestamp)
        .bind(revision - 1)
        .execute(&mut *tx).await.map_err(internal_error)?;
    if saved.rows_affected() == 0 {
        return Err(ApiError::new("conflict", "Draft ID is already in use"));
    }
    tx.commit().await.map_err(internal_error)?;
    Ok(DraftDocument {
        id,
        revision,
        base_recipe_id: base_recipe,
        base_revision,
        document,
        updated_at: timestamp,
    })
}

async fn api_get_draft(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DraftDocument>, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    get_draft(&state, &caller, &id).await.map(Json)
}

async fn get_draft(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
) -> Result<DraftDocument, ApiError> {
    let row = sqlx::query_as::<_, DraftRow>("SELECT id,revision,base_recipe_id,base_revision_id,document,updated_at FROM recipe_drafts WHERE id=? AND owner_token_hash=?")
        .bind(id).bind(&caller.token_hash).fetch_optional(&state.pool).await.map_err(internal_error)?
        .ok_or_else(|| ApiError::new("not_found", "Draft not found"))?;
    row.document()
}

async fn api_list_drafts(
    State(state): State<RecipeService>,
    headers: HeaderMap,
) -> Result<Json<Vec<DraftDocument>>, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    list_drafts(&state, &caller).await.map(Json)
}

async fn list_drafts(
    state: &RecipeService,
    caller: &CallerIdentity,
) -> Result<Vec<DraftDocument>, ApiError> {
    let rows = sqlx::query_as::<_, DraftRow>("SELECT id,revision,base_recipe_id,base_revision_id,document,updated_at FROM recipe_drafts WHERE owner_token_hash=? ORDER BY updated_at,id")
        .bind(&caller.token_hash).fetch_all(&state.pool).await.map_err(internal_error)?;
    rows.into_iter()
        .map(|row| row.document())
        .collect::<Result<Vec<_>, _>>()
}

async fn api_discard_draft(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<ExpectedDraftRevision>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(revision) = request?;
    discard_draft(&state, &caller, &id, revision).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn discard_draft(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    revision: ExpectedDraftRevision,
) -> Result<(), ApiError> {
    if revision.expected_revision <= 0 {
        return Err(ApiError::new(
            "invalid_request",
            "expected_revision must be positive",
        ));
    }
    let result =
        sqlx::query("DELETE FROM recipe_drafts WHERE id=? AND owner_token_hash=? AND revision=?")
            .bind(id)
            .bind(&caller.token_hash)
            .bind(revision.expected_revision)
            .execute(&state.pool)
            .await
            .map_err(internal_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(
            "conflict",
            "Draft not found or revision changed",
        ));
    }
    Ok(())
}

async fn api_publish_draft(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<DraftRevision>, JsonRejection>,
) -> Result<(StatusCode, Json<SaveResult>), ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(expected) = request?;
    let result = publish_draft(&state, &caller, &id, expected).await?;
    Ok((StatusCode::OK, Json(result)))
}

async fn publish_draft(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    expected: DraftRevision,
) -> Result<SaveResult, ApiError> {
    let owner_id = &caller.token_hash;
    let author = &caller.label;
    if expected.expected_revision <= 0 {
        return Err(ApiError::new(
            "invalid_request",
            "expected_revision must be positive",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    // Acquire SQLite's write lock before reading so concurrent publishers serialize
    // instead of both opening deferred read transactions and failing lock upgrade.
    let locked = sqlx::query(
        "UPDATE recipe_drafts SET revision=revision WHERE id=? AND owner_token_hash=? AND revision=?",
    )
    .bind(id)
    .bind(owner_id)
    .bind(expected.expected_revision)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    if locked.rows_affected() == 0 {
        return Err(ApiError::new(
            "conflict",
            "Draft not found or revision changed",
        ));
    }
    let row = sqlx::query_as::<_, DraftRow>("SELECT id,revision,base_recipe_id,base_revision_id,document,updated_at FROM recipe_drafts WHERE id=? AND owner_token_hash=?")
        .bind(id).bind(owner_id).fetch_optional(&mut *tx).await.map_err(internal_error)?
        .ok_or_else(|| ApiError::new("not_found", "Draft not found"))?;
    if row.revision != expected.expected_revision {
        return Err(ApiError::new("conflict", "The draft changed"));
    }
    if let (Some(recipe), Some(base)) = (&row.base_recipe_id, &row.base_revision_id) {
        let current: Option<String> =
            sqlx::query_scalar("SELECT current_revision_id FROM recipes WHERE id=? AND deleted=0")
                .bind(recipe)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal_error)?;
        if current.as_deref() != Some(base.as_str()) {
            let latest: Option<String> =
                sqlx::query_scalar("SELECT current_revision_id FROM recipes WHERE id=?")
                    .bind(recipe)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(internal_error)?;
            tx.rollback().await.map_err(internal_error)?;
            return Err(match latest {
                Some(revision) => {
                    revision_conflict(revision, "The recipe changed since this draft was started")
                }
                None => ApiError::new("not_found", "Recipe not found"),
            });
        }
    }
    let value: serde_json::Value = serde_json::from_str(&row.document).map_err(internal_error)?;
    let write: RecipeWrite = serde_json::from_value(value).map_err(|_| {
        ApiError::new(
            "invalid_request",
            "Draft is incomplete; title, servings, and body are required",
        )
    })?;
    validate_write(&write)?;
    let warnings = state.parse_body(&write.body)?;
    if let Some(reference) = expected.image_ref.as_deref() {
        let candidate: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM draft_cover_candidates WHERE image_ref=? AND draft_id=? AND owner_token_hash=? AND draft_revision=?",
        )
        .bind(reference)
        .bind(id)
        .bind(owner_id)
        .bind(expected.expected_revision)
        .fetch_optional(&mut *tx)
        .await
        .map_err(internal_error)?;
        if candidate.is_none() || !reference.starts_with("dimg_") {
            return Err(ApiError::new(
                "not_found",
                "Draft cover candidate not found",
            ));
        }
    }
    let (recipe_id, revision_id, image_ref) = if let (Some(recipe), Some(base)) =
        (&row.base_recipe_id, &row.base_revision_id)
    {
        let query = format!(
            "SELECT {REVISION_COLUMNS} FROM recipe_revisions r WHERE r.recipe_id=? AND r.id=?"
        );
        let current = sqlx::query_as::<_, RevisionRow>(&query)
            .bind(recipe)
            .bind(base)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| ApiError::new("not_found", "Recipe revision not found"))?;
        let revision = new_id("rev");
        let result = sqlx::query("UPDATE recipes SET current_revision_id=? WHERE id=? AND current_revision_id=? AND deleted=0")
            .bind(&revision).bind(recipe).bind(base).execute(&mut *tx).await.map_err(internal_error)?;
        if result.rows_affected() == 0 {
            let latest: Option<String> =
                sqlx::query_scalar("SELECT current_revision_id FROM recipes WHERE id=?")
                    .bind(recipe)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(internal_error)?;
            return Err(match latest {
                Some(revision) => revision_conflict(revision, "The recipe changed"),
                None => ApiError::new("not_found", "Recipe not found"),
            });
        }
        (
            recipe.clone(),
            revision,
            expected.image_ref.clone().or(current.image_ref),
        )
    } else {
        let recipe = new_id("rcp");
        let revision = new_id("rev");
        sqlx::query("INSERT INTO recipes(id,current_revision_id) VALUES(?,?)")
            .bind(&recipe)
            .bind(&revision)
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;
        (recipe, revision, expected.image_ref.clone())
    };
    insert_revision(
        &mut tx,
        &revision_id,
        &recipe_id,
        &now(),
        author,
        &write,
        false,
        image_ref.as_deref(),
    )
    .await?;
    if let Some(reference) = expected.image_ref.as_deref() {
        sqlx::query("DELETE FROM private_image_refs WHERE image_ref=?")
            .bind(reference)
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;
    }
    derive_revision(&mut tx, &revision_id, &write.body, author).await?;
    sqlx::query("DELETE FROM recipe_drafts WHERE id=? AND owner_token_hash=? AND revision=?")
        .bind(id)
        .bind(owner_id)
        .bind(expected.expected_revision)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    tx.commit().await.map_err(internal_error)?;
    let recipe = fetch_revision(&state.pool, &recipe_id, &revision_id)
        .await?
        .document();
    Ok(SaveResult { recipe, warnings })
}

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
    let caller = authorize_identity(&state, &headers).await?;
    let Json(request) = request?;
    append_state_revision(&state, &caller, &id, &request.base_revision, true, None)
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
    let caller = authorize_identity(&state, &headers).await?;
    let Json(request) = request?;
    append_state_revision(&state, &caller, &id, &request.base_revision, false, None)
        .await
        .map(Json)
}

async fn restore_revision(
    State(state): State<RecipeService>,
    Path((id, revision)): Path<(String, String)>,
    headers: HeaderMap,
    request: Result<Json<BaseRevision>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(request) = request?;
    append_state_revision(
        &state,
        &caller,
        &id,
        &request.base_revision,
        false,
        Some(&revision),
    )
    .await
    .map(Json)
}

async fn append_state_revision(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    base: &str,
    deleted: bool,
    restore: Option<&str>,
) -> Result<RecipeDocument, ApiError> {
    let author = &caller.label;
    let source = if let Some(revision) = restore {
        fetch_revision(&state.pool, id, revision).await?
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

async fn api_list_recipes(
    State(state): State<RecipeService>,
    headers: HeaderMap,
) -> Result<Json<Vec<RecipeDocument>>, ApiError> {
    authorize(&state, &headers).await?;
    list_recipes(&state).await.map(Json)
}

async fn list_recipes(state: &RecipeService) -> Result<Vec<RecipeDocument>, ApiError> {
    let query = format!("SELECT {REVISION_COLUMNS} FROM recipe_revisions r JOIN recipes p ON p.current_revision_id = r.id WHERE p.deleted = 0 ORDER BY r.title COLLATE NOCASE");
    let rows = sqlx::query_as::<_, RevisionRow>(&query)
        .fetch_all(&state.pool)
        .await
        .map_err(internal_error)?;
    Ok(rows.into_iter().map(RevisionRow::document).collect())
}

async fn api_get_recipe(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<RecipeDocument>, ApiError> {
    authorize(&state, &headers).await?;
    get_recipe(&state, &id).await.map(Json)
}

async fn get_recipe(state: &RecipeService, id: &str) -> Result<RecipeDocument, ApiError> {
    let row = fetch_current(&state.pool, id).await?;
    Ok(row.document())
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
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
enum InstructionPart {
    Text(String),
    Ingredient(String),
}

impl InstructionPart {
    fn text(&self) -> &str {
        match self {
            Self::Text(text) | Self::Ingredient(text) => text,
        }
    }
}

fn serialize_instruction_text<S>(
    steps: &[Vec<InstructionPart>],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    // Keep the public JSON API's array of plain strings.
    serializer.collect_seq(
        steps
            .iter()
            .map(|step| step.iter().map(InstructionPart::text).collect::<String>()),
    )
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
    #[serde(serialize_with = "serialize_instruction_text")]
    instructions: Vec<Vec<InstructionPart>>,
    notes: Option<String>,
    source_url: Option<String>,
}

fn scaled_view(
    state: &RecipeService,
    row: &RevisionRow,
    target: u32,
) -> Result<ScaledRecipe, ApiError> {
    scaled_view_with(state, row, target, |_, _, _| ()).map(|(view, ())| view)
}

fn scaled_view_with<T>(
    state: &RecipeService,
    row: &RevisionRow,
    target: u32,
    render: impl FnOnce(&cooklang::Recipe, &HashMap<usize, String>, &ScaledRecipe) -> T,
) -> Result<(ScaledRecipe, T), ApiError> {
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
    let declaration_notes =
        leading_declaration_block(&state.parser, &row.body, &recipe).unwrap_or_default();
    let declared_names = declaration_notes
        .keys()
        .filter_map(|index| recipe.ingredients.get(*index))
        .map(|ingredient| normalize(&ingredient.display_name()))
        .collect::<HashSet<_>>();
    let ingredients = recipe
        .ingredients
        .iter()
        .enumerate()
        .filter_map(|(index, ingredient)| {
            if ingredient.quantity.is_none()
                && ingredient.note.as_deref().is_none_or(str::is_empty)
                && !declaration_notes.contains_key(&index)
                && declared_names.contains(&normalize(&ingredient.display_name()))
            {
                return None;
            }
            let name = ingredient.display_name().into_owned();
            let note = match (
                ingredient.note.as_deref(),
                declaration_notes
                    .get(&index)
                    .filter(|note| !note.is_empty())
                    .map(String::as_str),
            ) {
                (Some(existing), Some(declaration)) if !existing.is_empty() => {
                    Some(format!("{existing}; {declaration}"))
                }
                (Some(existing), _) if !existing.is_empty() => Some(existing.to_owned()),
                (_, Some(declaration)) => Some(declaration.to_owned()),
                _ => None,
            };
            Some(ScaledIngredient {
                name,
                quantity: ingredient.quantity.as_ref().map(format_quantity),
                note,
            })
        })
        .collect();
    // Cooklang parses a leading declaration block as a step; omit only one
    // verified against standalone source lines.
    let instructions = method_content(&recipe, !declaration_notes.is_empty())
        .map(|(_, content)| match content {
            cooklang::Content::Step(step) => step
                .items
                .iter()
                .map(|item| match item {
                    cooklang::Item::Text { value } => InstructionPart::Text(value.clone()),
                    cooklang::Item::Ingredient { index } => InstructionPart::Ingredient(
                        recipe.ingredients[*index].display_name().into_owned(),
                    ),
                    cooklang::Item::Cookware { index } => {
                        InstructionPart::Text(recipe.cookware[*index].name.clone())
                    }
                    cooklang::Item::Timer { index } => {
                        let timer = &recipe.timers[*index];
                        InstructionPart::Text(match (&timer.name, &timer.quantity) {
                            (Some(name), Some(quantity)) => {
                                format!("{name} ({quantity})")
                            }
                            (Some(name), None) => name.clone(),
                            (None, Some(quantity)) => quantity.to_string(),
                            (None, None) => String::new(),
                        })
                    }
                    cooklang::Item::InlineQuantity { index } => {
                        InstructionPart::Text(recipe.inline_quantities[*index].to_string())
                    }
                })
                .collect(),
            cooklang::Content::Text(value) => vec![InstructionPart::Text(value.clone())],
        })
        .collect();
    let view = ScaledRecipe {
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
    };
    let extra = render(&recipe, &declaration_notes, &view);
    Ok((view, extra))
}

fn method_content(
    recipe: &cooklang::Recipe,
    has_declarations: bool,
) -> impl Iterator<Item = (Option<&str>, &cooklang::Content)> {
    recipe
        .sections
        .iter()
        .enumerate()
        .flat_map(move |(section_index, section)| {
            let mut title = section
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty());
            section
                .content
                .iter()
                .enumerate()
                .filter_map(move |(index, content)| {
                    if has_declarations && section_index == 0 && index == 0 {
                        return None;
                    }
                    let step_title = if matches!(content, cooklang::Content::Step(_)) {
                        title.take()
                    } else {
                        None
                    };
                    Some((step_title, content))
                })
        })
}

fn leading_declaration_block(
    parser: &CooklangParser,
    body: &str,
    recipe: &cooklang::Recipe,
) -> Option<HashMap<usize, String>> {
    let mut declarations = Vec::new();
    for line in body.lines().take_while(|line| !line.trim().is_empty()) {
        if line.trim() != line {
            return None;
        }
        let parsed = parser.parse(line).into_output()?;
        if parsed.sections.len() != 1
            || parsed.sections[0].name.is_some()
            || parsed.sections[0].content.len() != 1
            || parsed.ingredients.len() != 1
        {
            return None;
        }
        let cooklang::Content::Step(step) = &parsed.sections[0].content[0] else {
            return None;
        };
        let mut ingredient_index = None;
        let mut note = None;
        for item in &step.items {
            match item {
                cooklang::Item::Ingredient { index } if ingredient_index.is_none() => {
                    ingredient_index = Some(*index);
                }
                cooklang::Item::Text { value } if note.is_none() && is_prep_annotation(value) => {
                    let annotation = value.trim().strip_prefix(',')?.trim();
                    if !annotation.is_empty() {
                        note = Some(annotation.to_owned());
                    }
                }
                _ => return None,
            }
        }
        let source_ingredient = &parsed.ingredients[ingredient_index?];
        declarations.push((source_ingredient.display_name().into_owned(), note));
    }
    if declarations.len() < 2 {
        return None;
    }

    let first_step = match recipe.sections.first()?.content.first()? {
        cooklang::Content::Step(step) => step,
        cooklang::Content::Text(_) => return None,
    };
    let mut recipe_indices = Vec::new();
    for item in &first_step.items {
        match item {
            cooklang::Item::Ingredient { index } => recipe_indices.push(*index),
            cooklang::Item::Text { value } if is_prep_annotation(value) => {}
            _ => return None,
        }
    }
    if recipe_indices.len() != declarations.len() {
        return None;
    }

    let mut notes = HashMap::new();
    for (index, (name, note)) in recipe_indices.into_iter().zip(declarations) {
        if recipe.ingredients.get(index)?.display_name() != name {
            return None;
        }
        notes.insert(index, note.unwrap_or_default());
    }
    Some(notes)
}

fn is_prep_annotation(value: &str) -> bool {
    let value = value.trim();
    let Some(note) = value.strip_prefix(',') else {
        return value.is_empty();
    };
    matches!(
        note.trim().to_ascii_lowercase().as_str(),
        "diced" | "sliced" | "minced" | "bone-in" | "chopped"
    )
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

async fn api_validate_recipe(
    State(state): State<RecipeService>,
    headers: HeaderMap,
    request: Result<Json<ValidationRequest>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers).await?;
    let Json(request) = request?;
    validate_recipe(&state, request).await.map(Json)
}

async fn validate_recipe(
    state: &RecipeService,
    request: ValidationRequest,
) -> Result<serde_json::Value, ApiError> {
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
        if !image_ref.starts_with("img_")
            || !state.images.exists(image_ref).map_err(internal_error)?
        {
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
    let view = scaled_view(state, &row, target)?;
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
    Ok(serde_json::json!({
        "ingredients": parsed,
        "resolutions": resolutions,
        "warnings": warnings,
        "print_fit": {
            "fits": fits,
            "font_size_pt": font_size_pt,
            "status": status,
            "minimum_font_size_pt": 8
        }
    }))
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

async fn public_stylesheet() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../assets/site.css"),
    )
}

async fn public_font() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "font/ttf")],
        include_bytes!("../assets/Lora.ttf").as_slice(),
    )
}

fn public_page(title: &str, content: &str) -> Response {
    html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"color-scheme\" content=\"light\"><title>{}</title><link rel=\"stylesheet\" href=\"/assets/site.css\"></head><body><a class=\"skip-link\" href=\"#main\">Skip to content</a><header class=\"site-header\"><nav class=\"site-nav\" aria-label=\"Main navigation\"><a href=\"/recipes\">Recipes</a><a href=\"/ingredients\">Ingredients</a></nav></header><main id=\"main\">{}</main></body></html>",
        escape_html(title), content
    ))
}

async fn public_index(
    State(state): State<RecipeService>,
    Query(query): Query<PublicFilter>,
) -> Result<impl IntoResponse, ApiError> {
    let rows = public_rows(&state).await?;
    let filter = query.q.as_deref().unwrap_or("").trim().to_lowercase();
    let tag = query
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(normalize);
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
    let empty = if cards.is_empty() {
        "<p class=\"empty-notice\">No recipes match these filters.</p>"
    } else {
        ""
    };
    let clear = if query
        .q
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || query
            .tag
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        "<a class=\"text-link\" href=\"/recipes\">Clear filters</a>"
    } else {
        ""
    };
    let content = format!("<h1 class=\"collection-title\">Recipes</h1><section class=\"browse-panel\" aria-label=\"Search recipes\"><form class=\"search-form\" action=\"/recipes\" method=\"get\"><label>Search recipes <input type=\"search\" name=\"q\" value=\"{}\" placeholder=\"Recipe name\"></label><label>Tag <input name=\"tag\" value=\"{}\" placeholder=\"e.g. soup\"></label><button class=\"button\" type=\"submit\">Search</button>{clear}</form></section>{}<section class=\"recipe-grid\" aria-label=\"Recipes\">{}</section>", escape_html(query.q.as_deref().unwrap_or("")), escape_html(query.tag.as_deref().unwrap_or("")), empty, cards);
    Ok(public_page("Recipes", &content))
}

async fn public_ingredients(
    State(state): State<RecipeService>,
) -> Result<impl IntoResponse, ApiError> {
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT i.id, i.name, p.id, r.title
         FROM ingredients i
         LEFT JOIN ingredient_occurrences o ON o.ingredient_id = i.id
         LEFT JOIN recipes p ON p.current_revision_id = o.revision_id AND p.deleted = 0
         LEFT JOIN recipe_revisions r ON r.id = p.current_revision_id
         WHERE i.deleted = 0 AND i.redirect_to IS NULL
         GROUP BY i.id, p.id, r.title
         ORDER BY i.name COLLATE NOCASE, i.id, r.title COLLATE NOCASE, p.id",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(internal_error)?;
    let mut items: std::collections::BTreeMap<String, (String, Vec<(String, String)>)> =
        std::collections::BTreeMap::new();
    for (id, name, recipe_id, title) in rows {
        let item = items.entry(id).or_insert((name, Vec::new()));
        if let (Some(recipe_id), Some(title)) = (recipe_id, title) {
            item.1.push((recipe_id, title));
        }
    }
    let mut items: Vec<_> = items.into_iter().collect();
    items.sort_by(|(left_id, (left_name, _)), (right_id, (right_name, _))| {
        normalize(left_name)
            .cmp(&normalize(right_name))
            .then_with(|| left_id.cmp(right_id))
    });
    let entries = items
        .iter_mut()
        .map(|(_, (name, recipes))| {
            recipes.sort_by(|(left_id, left_title), (right_id, right_title)| {
                normalize(left_title)
                    .cmp(&normalize(right_title))
                    .then_with(|| left_id.cmp(right_id))
            });
            let count = recipes.len();
            let links = recipes
                .iter()
                .map(|(id, title)| {
                    format!(
                        "<li><a href=\"/recipes/{}\">{}</a></li>",
                        escape_html(id),
                        escape_html(title)
                    )
                })
                .collect::<String>();
            format!(
                "<article class=\"ingredient-card\"><h2>{}</h2><p class=\"usage-count\">{} {}</p>{}</article>",
                escape_html(name),
                count,
                if count == 1 { "recipe" } else { "recipes" },
                if links.is_empty() {
                    "<p class=\"quiet\">Not yet used in a recipe.</p>".to_owned()
                } else {
                    format!("<details class=\"ingredient-uses\"><summary>Browse recipes</summary><ul class=\"ingredient-recipes\">{links}</ul></details>")
                }
            )
        })
        .collect::<String>();
    let content = if entries.is_empty() {
        "<p class=\"empty-notice\">No ingredients yet.</p>"
    } else {
        &entries
    };
    let content = format!("<h1 class=\"collection-title\">Ingredients</h1><section class=\"ingredient-grid\" aria-label=\"Ingredients\">{content}</section>");
    Ok(public_page("Ingredients", &content))
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
    if is_pdf {
        let view = scaled_view(&state, &row, target)?;
        return render_pdf_response(&state, view).await;
    }
    let (view, (ingredients, instructions)) =
        scaled_view_with(&state, &row, target, |recipe, declarations, view| {
            recipe_web::render(recipe, declarations, view, state.parser.converter())
        })?;
    let image = view
        .image
        .as_ref()
        .map(|image| {
            format!(
                "<img class=\"recipe-cover\" src=\"{}\" alt=\"\">",
                escape_html(&image.url)
            )
        })
        .unwrap_or_default();
    let tags = view
        .tags
        .iter()
        .map(|tag| {
            format!(
                "<a class=\"tag\" href=\"/recipes?tag={}\">{}</a>",
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
                "<p class=\"recipe-source\"><a rel=\"noopener noreferrer\" href=\"{}\">View original source</a></p>",
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
                "<section class=\"recipe-section notes-section\"><h2>Notes</h2><p>{}</p></section>",
                escape_html(notes)
            )
        })
        .unwrap_or_default();
    let method_link = if instructions.is_empty() {
        ""
    } else {
        "<a href=\"#method\">Method</a>"
    };
    let method = if instructions.is_empty() {
        String::new()
    } else {
        format!("<section class=\"recipe-section method-section\" aria-labelledby=\"method\"><h2 id=\"method\" tabindex=\"-1\">Method</h2>{instructions}</section>")
    };
    let description = if view.description.is_empty() {
        String::new()
    } else {
        format!("<p class=\"lede\">{}</p>", escape_html(&view.description))
    };
    let content = format!(
        "<article class=\"recipe-detail\"><header class=\"recipe-hero\"><div class=\"recipe-heading\"><h1>{}</h1>{description}<p class=\"recipe-yield\">Serves {target}</p><nav class=\"recipe-jumps\" aria-label=\"Recipe sections\"><a href=\"#ingredients\">Ingredients</a>{method_link}</nav></div>{image}</header><div class=\"recipe-actions\"><form class=\"servings-form\" action=\"/recipes/{}#ingredients\" method=\"get\"><label for=\"servings\">Servings</label><div class=\"control-row\"><input id=\"servings\" type=\"number\" name=\"servings\" min=\"1\" max=\"1000\" value=\"{target}\"><button class=\"button\" type=\"submit\">Update servings</button></div></form><a class=\"button button-secondary\" href=\"/recipes/{}.pdf?servings={target}\">Printable PDF</a></div><div class=\"recipe-columns\"><section class=\"recipe-section ingredients-section\" aria-labelledby=\"ingredients\"><h2 id=\"ingredients\" tabindex=\"-1\">Ingredients</h2>{ingredients}</section>{method}</div>{notes}{source}<div class=\"tag-list recipe-tags\" aria-label=\"Recipe tags\">{tags}</div></article>",
        escape_html(&view.title), escape_html(id), escape_html(id)
    );
    Ok(public_page(&view.title, &content))
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
                "<a class=\"tag\" href=\"/recipes?tag={}\">{}</a>",
                query_component(tag),
                escape_html(tag)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<article class=\"recipe-card\"><a class=\"card-link\" href=\"/recipes/{}\"><div class=\"card-image\">{image}</div><div class=\"card-copy\"><h2 class=\"card-title\">{}</h2></div></a><div class=\"tag-list\">{tags}</div></article>",
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

async fn api_list_revisions(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<RecipeDocument>>, ApiError> {
    authorize(&state, &headers).await?;
    list_revisions(&state, &id).await.map(Json)
}

async fn list_revisions(state: &RecipeService, id: &str) -> Result<Vec<RecipeDocument>, ApiError> {
    let query = format!("SELECT {REVISION_COLUMNS} FROM recipe_revisions r WHERE r.recipe_id = ? ORDER BY r.created_at, r.id");
    let rows = sqlx::query_as::<_, RevisionRow>(&query)
        .bind(id)
        .fetch_all(&state.pool)
        .await
        .map_err(internal_error)?;
    if rows.is_empty() {
        return Err(ApiError::new("not_found", "Recipe not found"));
    }
    Ok(rows.into_iter().map(RevisionRow::document).collect())
}

async fn api_get_revision(
    State(state): State<RecipeService>,
    Path((id, revision)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<RecipeDocument>, ApiError> {
    authorize(&state, &headers).await?;
    get_revision(&state, &id, &revision).await.map(Json)
}

async fn get_revision(
    state: &RecipeService,
    id: &str,
    revision: &str,
) -> Result<RecipeDocument, ApiError> {
    Ok(fetch_revision(&state.pool, id, revision).await?.document())
}

const RECIPE_GUIDELINES: &str = include_str!("../guidelines/recipe-writing.md");

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
        RECIPE_GUIDELINES,
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

#[derive(Deserialize)]
struct GenerateDraftCover {
    expected_revision: i64,
    dish_description: String,
}

#[derive(Serialize)]
struct DraftCoverCandidate {
    image_ref: String,
    preview_url: String,
}

#[derive(Deserialize)]
struct GenerateRecipeCover {
    base_revision: String,
    dish_description: String,
    #[serde(default)]
    replace: bool,
}

async fn api_generate_draft_cover(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<GenerateDraftCover>, JsonRejection>,
) -> Result<(StatusCode, Json<DraftCoverCandidate>), ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(request) = request?;
    let result = generate_draft_cover(&state, &caller, &id, request).await?;
    Ok((StatusCode::CREATED, Json(result)))
}

async fn generate_draft_cover(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    request: GenerateDraftCover,
) -> Result<DraftCoverCandidate, ApiError> {
    let owner = &caller.token_hash;
    if request.expected_revision <= 0 {
        return Err(ApiError::new(
            "invalid_request",
            "expected_revision must be positive",
        ));
    }
    let generator = state
        .cover_generator
        .as_ref()
        .ok_or_else(|| ApiError::new("invalid_request", "Cover generation is not configured"))?;
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM recipe_drafts WHERE id=? AND owner_token_hash=?")
            .bind(id)
            .bind(owner)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal_error)?;
    match exists {
        None => return Err(ApiError::new("not_found", "Draft not found")),
        Some(revision) if revision != request.expected_revision => {
            return Err(ApiError::new("conflict", "The draft changed"))
        }
        _ => {}
    }
    let raw = generator
        .generate(&request.dish_description)
        .await
        .map_err(|message| ApiError::new("cover_generation_failed", message))?;
    let safe = tokio::task::spawn_blocking(move || normalize_generated_image(raw))
        .await
        .map_err(internal_error)?
        .map_err(|message| ApiError::new("invalid_image", message))?;
    let reference = new_id("dimg");
    // Commit the private guard before writing to shared storage. This durable guard
    // survives either a failed file write or a later candidate-row failure.
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    let locked = sqlx::query("UPDATE recipe_drafts SET revision=revision WHERE id=? AND owner_token_hash=? AND revision=?")
        .bind(id).bind(owner).bind(request.expected_revision).execute(&mut *tx).await.map_err(internal_error)?;
    if locked.rows_affected() == 0 {
        return Err(ApiError::new(
            "conflict",
            "The draft changed during cover generation",
        ));
    }
    sqlx::query("INSERT INTO private_image_refs(image_ref) VALUES(?)")
        .bind(&reference)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    tx.commit().await.map_err(internal_error)?;
    state
        .images
        .put(&reference, &safe)
        .map_err(internal_error)?;
    // Recheck after I/O; a changed/deleted draft leaves only a guarded orphan.
    let mut tx = state.pool.begin().await.map_err(internal_error)?;
    let locked = sqlx::query("UPDATE recipe_drafts SET revision=revision WHERE id=? AND owner_token_hash=? AND revision=?")
        .bind(id).bind(owner).bind(request.expected_revision).execute(&mut *tx).await.map_err(internal_error)?;
    if locked.rows_affected() == 0 {
        return Err(ApiError::new(
            "conflict",
            "The draft changed during cover generation",
        ));
    }
    sqlx::query("INSERT INTO draft_cover_candidates(image_ref,draft_id,owner_token_hash,draft_revision,created_at) VALUES(?,?,?,?,?)")
        .bind(&reference).bind(id).bind(owner).bind(request.expected_revision).bind(now())
        .execute(&mut *tx).await.map_err(internal_error)?;
    tx.commit().await.map_err(internal_error)?;
    Ok(DraftCoverCandidate {
        image_ref: reference.clone(),
        preview_url: format!("/api/v1/drafts/{id}/covers/{reference}"),
    })
}

async fn api_get_draft_cover(
    State(state): State<RecipeService>,
    Path((id, reference)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let bytes = get_draft_cover(&state, &caller, &id, &reference).await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, GENERATED_COVER_MIME),
            (axum::http::header::CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    )
        .into_response())
}

async fn get_draft_cover(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    reference: &str,
) -> Result<Vec<u8>, ApiError> {
    let owner = &caller.token_hash;
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT owner_token_hash,draft_revision FROM draft_cover_candidates WHERE image_ref=? AND draft_id=?",
    ).bind(reference).bind(id).fetch_optional(&state.pool).await.map_err(internal_error)?;
    let Some((candidate_owner, candidate_revision)) = row else {
        return Err(ApiError::new(
            "not_found",
            "Draft cover candidate not found",
        ));
    };
    let current_revision: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM recipe_drafts WHERE id=? AND owner_token_hash=?")
            .bind(id)
            .bind(owner)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal_error)?;
    if candidate_owner != *owner || current_revision != Some(candidate_revision) {
        return Err(ApiError::new(
            "not_found",
            "Draft cover candidate not found",
        ));
    }
    state.images.get(reference).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ApiError::new("not_found", "Image not found")
        } else {
            internal_error(error)
        }
    })
}

async fn api_generate_recipe_cover(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<GenerateRecipeCover>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(request) = request?;
    generate_recipe_cover(&state, &caller, &id, request)
        .await
        .map(Json)
}

async fn generate_recipe_cover(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    request: GenerateRecipeCover,
) -> Result<RecipeDocument, ApiError> {
    let generator = state
        .cover_generator
        .as_ref()
        .ok_or_else(|| ApiError::new("invalid_request", "Cover generation is not configured"))?;
    let current = fetch_revision(&state.pool, id, &request.base_revision).await?;
    ensure_current_base(&state.pool, id, &request.base_revision).await?;
    if current.image_ref.is_some() && !request.replace {
        return Err(ApiError::new(
            "conflict",
            "Recipe already has a cover; set replace=true to replace it",
        ));
    }
    let raw = generator
        .generate(&request.dish_description)
        .await
        .map_err(|message| ApiError::new("cover_generation_failed", message))?;
    let safe = tokio::task::spawn_blocking(move || normalize_generated_image(raw))
        .await
        .map_err(internal_error)?
        .map_err(|message| ApiError::new("invalid_image", message))?;
    let reference = new_id("dimg");
    sqlx::query("INSERT INTO private_image_refs(image_ref) VALUES(?)")
        .bind(&reference)
        .execute(&state.pool)
        .await
        .map_err(internal_error)?;
    state
        .images
        .put(&reference, &safe)
        .map_err(internal_error)?;
    append_image_revision(state, caller, id, &request.base_revision, Some(&reference)).await
}

const GENERATED_COVER_MIME: &str = "image/jpeg";

fn normalize_generated_image(bytes: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    if bytes.is_empty() || bytes.len() > 10 * 1024 * 1024 {
        return Err("Generated image must be non-empty and no larger than 10 MiB");
    }
    let mut reader = image::ImageReader::new(io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Generated image format is invalid")?;
    let format = reader.format().ok_or("Generated image format is invalid")?;
    if !matches!(
        format,
        image::ImageFormat::Jpeg | image::ImageFormat::Png | image::ImageFormat::WebP
    ) {
        return Err("Generated image format is not supported");
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8_192);
    limits.max_image_height = Some(8_192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| "Generated image is invalid")?;
    if image.width() as u64 * image.height() as u64 > 16_000_000 {
        return Err("Generated image exceeds 16 megapixels");
    }
    let thumbnail = image.thumbnail(1600, 1600);
    let mut output = io::Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut output, image::ImageFormat::Jpeg)
        .map_err(|_| "Generated image could not be normalized")?;
    if output.get_ref().len() > 10 * 1024 * 1024 {
        return Err("Generated image exceeds size limit");
    }
    Ok(output.into_inner())
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
    let is_draft_namespace = reference.starts_with("dimg_");
    if !reference.starts_with("img_") && !is_draft_namespace {
        return Err(ApiError::new("not_found", "Image not found"));
    }
    let private_candidate: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM private_image_refs WHERE image_ref=? LIMIT 1")
            .bind(&reference)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal_error)?;
    if private_candidate.is_some() || is_draft_namespace {
        let published: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM recipe_revisions WHERE image_ref=? LIMIT 1")
                .bind(&reference)
                .fetch_optional(&state.pool)
                .await
                .map_err(internal_error)?;
        if published.is_none() || (is_draft_namespace && private_candidate.is_some()) {
            return Err(ApiError::new("not_found", "Image not found"));
        }
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

async fn api_attach_image(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<AttachImage>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(request) = request?;
    attach_image(&state, &caller, &id, request).await.map(Json)
}

async fn attach_image(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    request: AttachImage,
) -> Result<RecipeDocument, ApiError> {
    if !request.image_ref.starts_with("img_")
        || !state
            .images
            .exists(&request.image_ref)
            .map_err(internal_error)?
    {
        return Err(ApiError::new("not_found", "Image not found"));
    }
    let private_candidate: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM private_image_refs WHERE image_ref=? LIMIT 1")
            .bind(&request.image_ref)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal_error)?;
    if private_candidate.is_some() {
        return Err(ApiError::new("not_found", "Image not found"));
    }
    append_image_revision(
        state,
        caller,
        id,
        &request.base_revision,
        Some(&request.image_ref),
    )
    .await
}

async fn detach_image(
    State(state): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<BaseRevision>, JsonRejection>,
) -> Result<Json<RecipeDocument>, ApiError> {
    let caller = authorize_identity(&state, &headers).await?;
    let Json(request) = request?;
    append_image_revision(&state, &caller, &id, &request.base_revision, None)
        .await
        .map(Json)
}

async fn append_image_revision(
    state: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    base: &str,
    image_ref: Option<&str>,
) -> Result<RecipeDocument, ApiError> {
    let author = &caller.label;
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
    if image_ref.is_some_and(|reference| reference.starts_with("dimg_")) {
        sqlx::query("DELETE FROM private_image_refs WHERE image_ref=?")
            .bind(image_ref)
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;
    }
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

async fn api_list_ingredients(
    State(s): State<RecipeService>,
    headers: HeaderMap,
    Query(q): Query<IngredientQuery>,
) -> Result<Json<Vec<IngredientSummary>>, ApiError> {
    authorize(&s, &headers).await?;
    list_ingredients(&s, q).await.map(Json)
}

async fn list_ingredients(
    s: &RecipeService,
    q: IngredientQuery,
) -> Result<Vec<IngredientSummary>, ApiError> {
    let query = q.q.as_deref().map(normalize);
    let rows = sqlx::query_as::<_, IngredientSummary>("SELECT i.id, i.name, COUNT(DISTINCT CASE WHEN p.deleted = 0 THEN p.id END) AS usage_count FROM ingredients i LEFT JOIN ingredient_occurrences o ON o.ingredient_id = COALESCE(i.redirect_to, i.id) LEFT JOIN recipe_revisions r ON r.id = o.revision_id LEFT JOIN recipes p ON p.current_revision_id = r.id WHERE i.deleted = 0 AND (? IS NULL OR i.normalized_name LIKE '%' || ? || '%') GROUP BY i.id ORDER BY i.name COLLATE NOCASE")
        .bind(&query).bind(&query).fetch_all(&s.pool).await.map_err(internal_error)?;
    Ok(rows)
}

#[derive(Serialize, FromRow)]
struct IngredientDetail {
    id: String,
    name: String,
    usage_count: i64,
}

async fn api_get_ingredient(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&s, &headers).await?;
    get_ingredient(&s, &id).await.map(Json)
}

async fn get_ingredient(s: &RecipeService, id: &str) -> Result<serde_json::Value, ApiError> {
    let row = sqlx::query_as::<_, IngredientDetail>("SELECT i.id, i.name, COUNT(DISTINCT p.id) AS usage_count FROM ingredients i LEFT JOIN ingredient_occurrences o ON o.ingredient_id = COALESCE(i.redirect_to, i.id) LEFT JOIN recipe_revisions r ON r.id = o.revision_id LEFT JOIN recipes p ON p.current_revision_id = r.id AND p.deleted = 0 WHERE i.id = ? AND i.deleted = 0 GROUP BY i.id")
        .bind(id).fetch_optional(&s.pool).await.map_err(internal_error)?.ok_or_else(|| ApiError::new("not_found", "Ingredient not found"))?;
    let aliases: Vec<String> = sqlx::query_scalar("SELECT alias FROM ingredient_aliases WHERE ingredient_id = ? ORDER BY alias COLLATE NOCASE").bind(id).fetch_all(&s.pool).await.map_err(internal_error)?;
    let recipes: Vec<String> = sqlx::query_scalar("SELECT DISTINCT p.id FROM ingredient_occurrences o JOIN recipe_revisions r ON r.id=o.revision_id JOIN recipes p ON p.current_revision_id=r.id WHERE o.ingredient_id=? AND p.deleted=0 ORDER BY p.id").bind(id).fetch_all(&s.pool).await.map_err(internal_error)?;
    Ok(
        serde_json::json!({"id":row.id,"name":row.name,"usage_count":row.usage_count,"aliases":aliases,"recipe_ids":recipes}),
    )
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

async fn api_rename_ingredient(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<NameBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let caller = authorize_identity(&s, &headers).await?;
    let Json(b) = request?;
    rename_ingredient(&s, &caller, &id, b).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rename_ingredient(
    s: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    b: NameBody,
) -> Result<(), ApiError> {
    let name = b.name.trim();
    let normalized = normalize(name);
    if normalized.is_empty() {
        return Err(ApiError::new("invalid_request", "name must not be empty"));
    }
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let collision: Option<String>=sqlx::query_scalar("SELECT id FROM ingredients WHERE normalized_name=? AND id<>? UNION SELECT ingredient_id FROM ingredient_aliases WHERE normalized_alias=?").bind(&normalized).bind(id).bind(&normalized).fetch_optional(&mut *tx).await.map_err(internal_error)?;
    if collision.is_some() {
        return Err(ApiError::new(
            "conflict",
            "ingredient name is already in use",
        ));
    }
    let old: Option<String> =
        sqlx::query_scalar("SELECT name FROM ingredients WHERE id=? AND deleted=0")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal_error)?;
    let old = old.ok_or_else(|| ApiError::new("not_found", "Ingredient not found"))?;
    sqlx::query("UPDATE ingredients SET name=?, normalized_name=? WHERE id=?")
        .bind(name)
        .bind(&normalized)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query("INSERT INTO ingredient_aliases(normalized_alias,alias,ingredient_id) VALUES(?,?,?) ON CONFLICT(normalized_alias) DO UPDATE SET alias=excluded.alias, ingredient_id=excluded.ingredient_id").bind(normalize(&old)).bind(&old).bind(id).execute(&mut *tx).await.map_err(internal_error)?;
    history(&mut tx, id, "rename", name, "{}", &caller.label).await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(())
}

async fn api_add_alias(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<AliasBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let caller = authorize_identity(&s, &headers).await?;
    let Json(b) = request?;
    add_alias(&s, &caller, &id, b).await?;
    Ok(StatusCode::CREATED)
}

async fn add_alias(
    s: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    b: AliasBody,
) -> Result<(), ApiError> {
    let alias = b.alias.trim();
    let normalized = normalize(alias);
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let exists: Option<String> =
        sqlx::query_scalar("SELECT id FROM ingredients WHERE id=? AND deleted=0")
            .bind(id)
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
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    history(&mut tx, id, "alias_added", alias, "{}", &caller.label).await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(())
}

async fn api_remove_alias(
    State(s): State<RecipeService>,
    Path((id, alias)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let caller = authorize_identity(&s, &headers).await?;
    remove_alias(&s, &caller, &id, &alias).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_alias(
    s: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    alias: &str,
) -> Result<(), ApiError> {
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let result =
        sqlx::query("DELETE FROM ingredient_aliases WHERE normalized_alias=? AND ingredient_id=?")
            .bind(normalize(alias))
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new("not_found", "Alias not found"));
    }
    history(&mut tx, id, "alias_removed", alias, "{}", &caller.label).await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(())
}

async fn api_merge_ingredient(
    State(s): State<RecipeService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Result<Json<MergeBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let caller = authorize_identity(&s, &headers).await?;
    let Json(b) = request?;
    merge_ingredient(&s, &caller, &id, b).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn merge_ingredient(
    s: &RecipeService,
    caller: &CallerIdentity,
    id: &str,
    b: MergeBody,
) -> Result<(), ApiError> {
    let survivor = normalize(&b.survivor);
    let mut tx = s.pool.begin().await.map_err(internal_error)?;
    let source: Option<String> =
        sqlx::query_scalar("SELECT name FROM ingredients WHERE id=? AND deleted=0")
            .bind(id)
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
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query("UPDATE ingredient_occurrences SET ingredient_id=? WHERE ingredient_id=?")
        .bind(&target)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query("UPDATE ingredient_aliases SET ingredient_id=? WHERE ingredient_id=?")
        .bind(&target)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    sqlx::query(
        "UPDATE ingredient_resolution_provenance SET ingredient_id=? WHERE ingredient_id=?",
    )
    .bind(&target)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    sqlx::query("UPDATE ingredients SET deleted=1, redirect_to=? WHERE id=?")
        .bind(&target)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    history(
        &mut tx,
        id,
        "merged",
        &source,
        &serde_json::json!({"survivor":target}).to_string(),
        &caller.label,
    )
    .await?;
    history(
        &mut tx,
        &target,
        "merge_received",
        &source,
        &serde_json::json!({"source":id}).to_string(),
        &caller.label,
    )
    .await?;
    tx.commit().await.map_err(internal_error)?;
    Ok(())
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

async fn api_list_tags(
    State(s): State<RecipeService>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    authorize(&s, &headers).await?;
    list_tags(&s).await.map(Json)
}

async fn list_tags(s: &RecipeService) -> Result<Vec<serde_json::Value>, ApiError> {
    let rows:Vec<(String,i64)>=sqlx::query_as("SELECT j.value,COUNT(DISTINCT p.id) FROM recipes p JOIN recipe_revisions r ON r.id=p.current_revision_id JOIN json_each(r.tags) j WHERE p.deleted=0 GROUP BY j.value ORDER BY j.value COLLATE NOCASE").fetch_all(&s.pool).await.map_err(internal_error)?;
    Ok(rows
        .into_iter()
        .map(|(tag, count)| serde_json::json!({"tag":tag,"count":count}))
        .collect())
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

async fn api_search_recipes(
    State(s): State<RecipeService>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    authorize(&s, &headers).await?;
    search_recipes(&s, q).await.map(Json)
}

async fn search_recipes(
    s: &RecipeService,
    q: SearchQuery,
) -> Result<Vec<serde_json::Value>, ApiError> {
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
    Ok(result)
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
    Ok(revision_conflict(current_revision_id, message))
}

fn revision_conflict(current_revision_id: String, message: &str) -> ApiError {
    let mut error = ApiError::new("conflict", message);
    error.current_revision_id = Some(current_revision_id);
    error
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
