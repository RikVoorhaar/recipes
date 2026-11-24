# Recipe App

The v1 architecture and behavior are specified in [recipe-app-v1-spec.md](recipe-app-v1-spec.md); implementation is sequenced in [recipe-app-v1-implementation-plan.md](recipe-app-v1-implementation-plan.md).

## Requirements and quick start

Install the stable Rust toolchain with the `rustfmt` and `clippy` components. From a clean checkout:

```sh
cp config.example.toml config.toml
cargo run -p recipe-app
```

The app creates the configured database parent and content directory. SQLx migrations run automatically at startup before the HTTP listener accepts requests.

Check `http://127.0.0.1:3000/health`; it returns `200` and `{"status":"ok"}`. The database path and content directory are configurable, so smoke tests and local runs should use isolated temporary paths rather than shared data.

## Configuration

Settings are loaded in increasing precedence: built-in defaults, the TOML file selected by `APP_CONFIG_FILE` (default `config.toml`), then environment variables. Environment variable names use the `APP__` prefix and double-underscore nesting, for example `APP__BIND_ADDR`, `APP__DATABASE_PATH`, and `APP__CONTENT_DIR`. `api_tokens` may be configured in TOML or as a comma-separated `APP__API_TOKENS` value; comma splitting is applied only to this token-list setting. Configuration load/parse failures use a generic diagnostic rather than echoing configuration values. Do not commit real tokens or log settings; use a private config file or secret environment injection. Token validation/storage is implemented with the API stage, not this bootable foundation.

## Quality gates and tests

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs all three commands. The local pre-commit equivalent is documented/configured with [pre-commit](https://pre-commit.com/): run `pre-commit install` once, then the `rust-quality-gates` hook runs `scripts/check.sh`. Without pre-commit, run that script directly.

The HTTP integration-test helper in `recipe-app/tests/support` provides a temporary database and content directory, starts an actual ephemeral HTTP listener, and can restart against the same database.

## Recipe authoring API (Stage 2)

All `/api/v1` recipe and guideline endpoints require a bearer token, including reads. `/health` and `/api/v1/openapi.yaml` are public. Configure tokens as `label:secret` entries, for example `api_tokens = ["mcp:use-a-long-random-secret"]`; use a unique high-entropy secret per client. The label is stored with the SHA-256 token digest and appears as the revision author; the secret itself is never persisted. Authentication scans configured digest rows using constant-time comparison. Setting `APP__API_TOKENS` overrides the TOML list (comma-separated `label:secret` entries).

The initial recipe endpoints are documented in [openapi.yaml](openapi.yaml). Create with `POST /api/v1/recipes`; update with `PUT /api/v1/recipes/{id}` and include the `base_revision` from the fetched document. A stale base gets `409` and `current_revision_id`. Delete takes `{ "base_revision": "..." }` as JSON; restore from a soft delete uses `POST /api/v1/recipes/{id}/restore`, and historical restore uses `POST /api/v1/recipes/{id}/revisions/{revision}`. Every state change adds an immutable revision. `GET /api/v1/recipes` omits deleted records, while `GET /api/v1/recipes/{id}` and history remain available for recovery.

Cooklang is parsed with `cooklang-rs`. Structured metadata is authoritative: both YAML front matter and legacy `>>` metadata are rejected rather than stripped. The parser runs first so malformed Cooklang/YAML still returns its parser diagnostics. Ingredient-list-only bodies are valid as consecutive ingredient references, with no prose instructions. Successful writes return parser warnings; parse errors use a structured error envelope with parser diagnostics. The shared writer guidance is checked in at [recipe-app/guidelines/recipe-writing.md](recipe-app/guidelines/recipe-writing.md) and served as Markdown at `/api/v1/guidelines`.

## Ingredient registry and search (Stage 3)

Each saved recipe revision has immutable derived ingredient occurrences parsed from Cooklang. Names are looked up by Unicode default case-folding, trimming and whitespace collapsing; no stemming or inferred synonyms are used. Unknown names create canonical ingredients automatically. Occurrence wording is retained from the body. Cooklang inline display aliases do not determine registry identity. A durable per-revision resolution provenance record preserves the canonical ID selected when the revision was saved, even if that alias is later removed.

Use `/api/v1/ingredients` to list/search canonical names and their usage counts, and `/api/v1/ingredients/{id}` for aliases and linked current recipe IDs. Usage counts are distinct, current, non-deleted recipes. Alias add/remove, rename, and merge are authenticated operations. Search ingredient filters resolve only canonical IDs, canonical names, and currently registered aliases; raw occurrence wording is never treated as a search identity. Removing an alias immediately makes the alias string unresolvable as a filter, but does not alter saved occurrence links. Names and aliases are unique after normalization. Merge accepts a survivor ID, canonical name, or alias; it moves occurrence links and provenance to the survivor and retains the source as a redirecting/deleted record. Incoming redirects are flattened on merge, so names and IDs continue to resolve consistently across chained merges. Content-preserving recipe revisions (delete and either restore path) copy ingredient resolution provenance instead of resolving the body against today’s aliases. Ingredient history is retained separately from recipe revision history.

`GET /api/v1/tags` returns distinct current non-deleted recipe counts. `GET /api/v1/search` accepts comma-separated `ingredients`, `exclude_ingredients`, `tags`, and `exclude_tags`; ingredient and tag modes are `all` (default) or `any`. Ingredient filters resolve to canonical IDs and result `matched_ingredients` contains occurrence wording for the included canonical IDs. It also accepts `q`, `min_servings`, `max_servings`, `has_image`, `include_deleted`, `updated_since`, `sort`, `limit`, and `offset`. Results contain ID, title, tags, matched ingredient wording, and a short body snippet. Text is a case-insensitive substring search over title, description, notes and body. Ordering defaults to title; `updated` sorts newest first and `relevance` puts title matches first; ties are deterministic by title then ID. `updated_since` is RFC3339 and compares instants. Limit defaults to 50 and is capped at 200. `has_image` filters using the current revision's attachment.

Ingredient mutations consume typed JSON objects: rename takes `{ "name": "..." }`, alias add takes `{ "alias": "..." }`, and merge takes `{ "survivor": "ingredient ID, canonical name, or alias" }`.

Startup automatically backfills and rebuilds derived occurrences after migrations, including when upgrading a populated Stage 2 database. Run `cargo run -p recipe-app -- rebuild-derived` to explicitly rebuild occurrence links from every retained recipe revision. It is transactional, uses saved resolution provenance before registry lookup, preserves rename/merge identity and history, and creates canonical records for unmatched legacy occurrences. `updated_since` accepts RFC3339 timestamps with offsets; comparisons and `updated` sorting use instants, not timestamp strings.

## Image storage and uploads (Stage 4)

Images are stored under the configured `content_dir` through the `ImageStorage` trait; references are opaque `img_…` IDs and never contain paths. Authenticated `POST /api/v1/images` accepts raw JPEG, PNG, or WebP bytes up to 10 MiB, and rejects a declared MIME type that does not match the decoded format. Decoder limits cap each dimension at 8,192 pixels and each decoder allocation at 64 MiB; the decoded image is additionally capped at 16 megapixels. The server makes a maximum-1600-pixel thumbnail and re-encodes it as JPEG. Animated images are reduced to a still image. The response is `{ "ref": "img_…", "url": "/images/img_…" }`; `GET /images/{ref}` is public and backend-independent.

Attach with authenticated `POST /api/v1/recipes/{id}/image` and `{ "base_revision": "rev_…", "image_ref": "img_…" }`; detach with `DELETE` on the same path and `{ "base_revision": "rev_…" }`. Both operations append immutable revisions only if the supplied base revision is current. Ordinary content edits, delete/restore, and historical restore preserve the image in their copied revision. Detaching or replacing an image never deletes stored bytes: older revisions remain available through the API and their image URLs continue to work.

`ImageStorage` exposes `put`, `get`, `exists`, and a maintenance-only `delete`. Its reusable contract test exercises round trips, missing keys, idempotent deletion, and path traversal rejection against the filesystem backend.

## Local MCP adapter (Stage 6)

`recipe-mcp` is a standalone stdio MCP server. It calls the authenticated HTTP API (it does not parse Cooklang or implement recipe behavior). Build/install it with:

```sh
cargo install --path recipe-mcp --locked
```

Set the URL and token in the harness process environment; never put a real token in a checked-in client file. Example Claude Desktop-style configuration (replace the token with your private API token):

```json
{
  "mcpServers": {
    "recipes": {
      "command": "recipe-mcp",
      "env": {
        "RECIPE_API_BASE_URL": "http://127.0.0.1:3000",
        "RECIPE_API_TOKEN": "replace-with-your-private-token"
      }
    }
  }
}
```

The executable also runs from a checkout with `RECIPE_API_BASE_URL=http://127.0.0.1:3000 RECIPE_API_TOKEN=... cargo run -p recipe-mcp`. It emits newline-delimited MCP JSON-RPC only on stdout; startup diagnostics go to stderr. Tools include recipe search/CRUD and revision recovery, local-file image upload and attach/detach, ingredient operations, tags, and validation. `recipe://guidelines` and `get_guidelines` return the same API-served writing guide. Read it before authoring or updating. Updates require `base_revision`; API conflict and validation responses are returned as actionable tool errors.

The automated stdio/API integration test uses an ephemeral real API, temporary SQLite database/content path and a local image fixture generated in the test:

```sh
cargo test -p recipe-mcp --test stdio
```

### Stage 7 live acceptance (passed 2026-11-24)

The live local-agent vertical slice was run against a disposable local app and database. The [Terra reviewer follow-up in the evidence](docs/stage7-live-acceptance.md#terra-reviewer-follow-up-agent-owned-setup-and-public-checks) records the agent-owned locked build/install, MCP registration, MCP interactions, public HTTP checks, and `lopdf` verification from the same OpenCode session. The earlier operator-owned setup is retained only as historical context. The deployment, backup, and restore runbook is [docs/operations.md](docs/operations.md).

The acceptance used OpenCode with OpenAI GPT-6 Luna and the locally installed `recipe-mcp` stdio adapter. The agent passed guidelines, create, local image upload/attach, update, stale `409`, prior-revision retrieval, and alias-plus-tag search through MCP. The agent also checked the public index, scaled detail, image, and downloaded PDF through its own shell tool; it used `lopdf` to assert a one-page A4 result. The isolated quality gates also passed:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo install --path recipe-mcp --locked
```

Then start a disposable app instance in one terminal:

```sh
APP__BIND_ADDR=127.0.0.1:3000 \
APP__DATABASE_PATH=/tmp/recipe-stage7.sqlite \
APP__CONTENT_DIR=/tmp/recipe-stage7-content \
APP__API_TOKENS='stage7:replace-with-a-private-random-token' \
cargo run -p recipe-app
```

Register the installed `recipe-mcp` in a real local agent harness using `RECIPE_API_BASE_URL=http://127.0.0.1:3000` and the same token. Through MCP only, record the redacted transcript: read guidelines; create a recipe and upload/attach an image; update it; show a rejected stale update and retrieve the previous revision; add an alias and find it with a tag filter. After MCP returns the public recipe ID, verify the public page and PDF from a second terminal:

```sh
curl -f http://127.0.0.1:3000/recipes/$RECIPE_ID
curl -f "http://127.0.0.1:3000/recipes/$RECIPE_ID.pdf?servings=4" -o /tmp/recipe-stage7.pdf
pdfinfo /tmp/recipe-stage7.pdf | grep -E 'Pages:|Page size:'
```

Confirm the PDF reports one page and A4 dimensions. Do not use production data or check in the token. See the acceptance evidence for the recorded run and results.
