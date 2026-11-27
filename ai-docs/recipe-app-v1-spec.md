# Recipe App — v1 Implementation Spec

Status: historical v1 design; superseded by the current implementation and README.
This document describes the v1 REST API and is not current guidance.

## 1. Purpose

A personal family recipe book:

- A **public, read-only website** for browsing recipes, their pictures, scaling servings, and printing a one-page recipe sheet.
- An **authenticated HTTP API** that owns all writes and offers rich structured search, designed primarily for AI agents.
- A **local MCP server** that exposes the API to agentic sessions, including recipe-writing guidance.

The recipes are written for an experienced cook. The printed pages must also work for young children who cannot read, so pictures matter.

### v1 target scenario (the vertical slice)

1. In a local agentic coding session, the agent installs and configures the MCP server against a running instance.
2. Through MCP, the agent reads the writing guidelines, creates a recipe (Cooklang body + metadata), and attaches an image.
3. On the public website, the recipe appears; a visitor opens it, changes servings, and sees quantities rescale.
4. The visitor downloads a single-page A4 PDF with the picture.
5. The agent updates the recipe through MCP; an update based on an outdated revision is rejected; the previous revision remains retrievable via the API.
6. The agent finds the recipe through structured search by canonical ingredient (including via an alias) and tag.

## 2. Guiding principles

- **Specify behavior, not schema.** Any schema shown here is illustrative. Agents may change storage design freely as long as the guarantees in this spec hold.
- **One implementation of recipe logic.** Website, API and MCP share the same domain/service layer. MCP is an adapter, not a second implementation.
- **Nothing is destroyed.** Edits create revisions; deletes are soft.
- **Derived data is rebuildable.** Parsed ingredients and search indexes can be regenerated from stored revisions.
- **Linting and formatting from day one.** `rustfmt` and `clippy` (warnings as errors) in CI/pre-commit; same for any JS/CSS/Typst tooling used.

## 3. Architecture

| Area | Decision |
|---|---|
| Language | Rust, single portable application deployed to an existing VPS |
| Web | Axum on Tokio; server-rendered HTML (Askama or equivalent); HTMX/minimal JS only where useful |
| Database | SQLite via SQLx, with migrations |
| Recipe format | Cooklang, parsed with `cooklang-rs` |
| PDF | Typst, compiled in-process via the Rust crates (no external CLI dependency at runtime if feasible) |
| Images | Local content directory in v1, behind a storage trait |
| MCP | Separate local MCP server binary (stdio transport) that calls the HTTP API with a token |

Implementation freedom: crate choices other than Cooklang/Typst/SQLite/Rust, module layout, and whether MCP lives in the same workspace (recommended: same Cargo workspace, separate binary).

## 4. Recipe model

### 4.1 Conceptual split

| Part | Contents | Notes |
|---|---|---|
| Metadata | Title, short description, base servings/yield, tags, image reference (optional), source URL (optional), adaptation notes (optional) | Structured fields, part of the API JSON document |
| Body | Cooklang text: ingredients with quantities, cookware, timers, concise instructions | The **only** place ingredient quantities are authored |
| Bookkeeping | Stable recipe ID, revision ID, revision timestamp, deleted state, author/token label | Application-owned, never in the body |
| Derived | Parsed ingredient occurrences, canonical ingredient links, search index | Rebuildable |

Rules:

- Metadata has exactly one home. Do not duplicate title, servings or tags in Cooklang front matter. If the body contains Cooklang metadata that conflicts with structured metadata, the API rejects it (or the implementation strips/forbids it — pick one and document it).
- Ingredient lists are always derived from the body, never authored separately.
- A recipe may consist mostly of an ingredient list with very short or no instructions. The implementing agent should find the cleanest valid Cooklang representation for this and document it in the writing guidelines.
- The body must parse without errors to be saved. Parser warnings are returned to the caller.

### 4.2 Illustrative API document (not a schema commitment)

```json
{
  "id": "rcp_01H...",
  "revision": "rev_01H...",
  "revision_created_at": "2026-11-24T12:00:00Z",
  "title": "Stir-fried noodles with vegetables",
  "description": "Weeknight wok noodles.",
  "servings": 4,
  "tags": ["weeknight", "asian", "vegetarian"],
  "image": { "ref": "img_01H...", "url": "/images/img_01H..." },
  "source_url": null,
  "notes": null,
  "body": "Boil @egg noodles{250%g}, rinse cold.\nHeat #wok to ~250 °C surface, stir-fry @mixed vegetables{400%g} ...",
  "deleted": false
}
```

### 4.3 Revisions and soft deletes

Required guarantees:

- A recipe has a stable ID across all edits.
- Every successful write (metadata, body, image change, delete, restore) creates a new immutable revision with a timestamp. The previous revision becomes non-current; it is never modified or removed.
- Updates must include the revision ID the client based its edit on. If it is not the current revision, the API returns `409 Conflict` with the current revision ID. No silent overwrites.
- Delete = soft delete. Deleted recipes are hidden from the public site and default search, but remain retrievable (and restorable) through the API with full history.
- Restoring an old revision creates a new revision whose content equals the old one.
- Revision history is **API-only**. The public site shows only the current revision of non-deleted recipes.
- Images referenced by any retained revision must never be removed from storage.

The same history/soft-delete principle applies to canonical ingredients and aliases (see §5). Derived indexes are not versioned.

## 5. Canonical ingredients and aliases

Part of v1.

Behavior:

1. On save, extract every ingredient from the parsed Cooklang body.
2. Normalize the name (case-fold, trim, collapse whitespace). Do not stem or guess.
3. Resolve against canonical ingredient names and the alias registry.
4. If no match exists, **automatically create** a new canonical ingredient with that name.
5. Store the link from each occurrence to its canonical ingredient ID, while preserving the original wording for display.

API operations:

- List/search canonical ingredients (with usage counts).
- Add/remove aliases (e.g. `tomatoes` → `tomato`).
- Rename a canonical ingredient.
- Merge two canonical ingredients (all aliases and recipe links move to the survivor; the merged one is retained as a soft-deleted/redirecting record).

Rules:

- Never auto-merge similar names. `tomatoes` → `tomato` happens only via an explicit alias. `cherry tomatoes` stays distinct unless explicitly merged.
- Alias and merge changes take effect on search immediately (re-resolve links or resolve at query time — implementation choice), without creating recipe revisions.
- An alias name must be unique and cannot equal another canonical name.
- Cooklang's inline display aliases (e.g. `@tomatoes|tomato{}` style syntax) affect display only; the application registry is the authority for identity.

## 6. Servings and quantity scaling

- All quantities come from Cooklang. Use `cooklang-rs` scaling and quantity formatting rather than a custom implementation wherever it suffices.
- The website and the PDF use the same scaling and formatting pipeline.
- Displayed servings are a view parameter (e.g. `?servings=6`) and never create revisions. Changing the saved base servings does.
- Format results naturally: prefer common fractions (½, ⅓, ¼, ¾) and sensible precision for metric units; avoid outputs like `1.3333 eggs`. A displayed value like `1⅓ eggs` is acceptable.
- Never silently round to a materially different amount. Fixed (non-scaling) quantities supported by Cooklang must be respected.
- Unit conversion is out of scope for v1 beyond what is needed for clean display.

## 7. Public website (read-only)

No accounts, no login, no editing controls.

Pages:

- **Index**: grid of recipes with picture (or tasteful placeholder), title, tags. Picture-first, so children can choose a dish. Basic title filter and tag filter are fine; elaborate human search is out of scope.
- **Recipe page**: picture, title, description, servings control, scaled ingredient list, instructions, notes, source link, “Download PDF” button.
- **Tag page/filter**: recipes with a given tag.
- **PDF endpoint**: `GET /recipes/{id}.pdf?servings=N`.
- Images served from a public endpoint that hides the storage backend.

Requirements: mobile-friendly and tablet-friendly layout; readable serif-friendly typography; fast server-rendered pages; works without JavaScript except optional enhancements (e.g. live servings change via HTMX).

## 8. Authenticated HTTP API

- JSON over HTTP, versioned prefix (e.g. `/api/v1`).
- Auth: bearer token(s) configured on the server (env/config file); tokens hashed at rest; constant-time comparison. Read endpoints of the API may also require a token (simpler, consistent); the public website is the unauthenticated read path.
- All writes go through the API. There are no write paths on the public site.
- Errors are structured and agent-friendly: machine code, human message, and for Cooklang errors the line/column and parser message.
- Provide an OpenAPI description (generated or hand-written) so agents can call the API directly.

Required operations:

| Area | Operations |
|---|---|
| Recipes | create, get current, update (with base revision), soft delete, restore, list |
| History | list revisions, get a specific revision, restore a revision |
| Images | upload (multipart or raw bytes; validate type, size limit, re-encode/resize to sensible web size), attach/detach to recipe (creates a revision) |
| Ingredients | list/search, get with linked recipes, add/remove alias, rename, merge |
| Tags | list with counts |
| Search | structured search (below) |
| Guidelines | get recipe-writing guidelines (same text the MCP exposes) |
| Validation | dry-run: parse a body and return parsed ingredients, canonical resolution preview, warnings, and print-fit result without saving |
| Rendering | get the scaled, parsed recipe as JSON; get PDF |

### 8.1 Structured search (agent-focused)

Composable filters, all optional:

- Free text over title, description, notes and body.
- Ingredients: canonical IDs or names (resolved through aliases), with `all` / `any` semantics, plus an `exclude` list.
- Tags: `all` / `any` / `exclude`.
- Servings range; has-image; include-deleted (default false); updated-since.
- Sort by relevance, title, updated time; pagination.

Results return IDs, titles, tags, matched ingredients and a short snippet, so an agent can decide what to fetch. SQLite FTS5 is the suggested free-text engine.

## 9. MCP server

- A standalone local binary using stdio transport, configured by environment: API base URL + token.
- Calls the HTTP API; contains no recipe logic of its own.
- Tools mirror the API: search recipes, get recipe, create, update (requires base revision), delete, restore, list/get/restore revisions, upload/attach image (from a local file path), ingredient list/alias/rename/merge, list tags, validate body.
- Exposes the writing guidelines both as an MCP resource and as a `get_guidelines` tool; the create/update tool descriptions instruct the agent to read them first.
- README section with copy-paste install and client configuration for a typical local agent harness.

## 10. Recipe-writing guidelines

Stored in the repository as a Markdown document, served by the API and MCP. Initial content:

- Metric quantities with explicit units. All quantities in Cooklang syntax.
- A clear base servings or yield.
- Every ingredient marked as a Cooklang ingredient where it is used, so it lands in the derived list and the canonical registry.
- Prefer existing canonical ingredient names (search the ingredient list before writing); add aliases rather than inventing near-duplicates.
- **Temperatures are explicit whenever a useful measurement exists.** The cook has an infrared thermometer and a Thermapen and uses them constantly:
  - Oven: temperature and fan/conventional mode.
  - Meat, fish, baked goods, custards etc.: target core temperature.
  - Pans/woks/griddles: target surface temperature when it matters (very hot or deliberately cool).
  - Liquids and starches: target temperature and hold time where relevant (e.g. "bring to 80 °C and hold 2 min").
  - State what to measure and where (core, thickest part, surface).
- **Concise, for an experienced cook.** Omit mundane steps ("stir-fry the vegetables"). Include special techniques and surprising steps, in as few words as possible ("boil noodles, rinse cold").
- Many everyday dishes can be just a picture and an ingredient list with one or two key notes.
- Must fit on one printed page; if the validator reports it does not fit, shorten the recipe — do not split it.
- Optional source URL and adaptation notes.
- Never invent times, temperatures or other precision when importing a recipe; only add a temperature you are confident is correct, and you may propose one where useful.

## 11. Printing

- Page size **A4**, exactly **one page** per recipe. Multi-page output is never an accepted result.
- Includes the picture when present (prominent; the page should work as a family recipe book). Clean layout without picture otherwise.
- Contents: picture, title, servings (as requested), ingredient list with scaled quantities, instructions, key temperatures readable at a glance, short notes. Source URL in small print is fine.
- Typst template lives in the repository.
- Auto-fit: start at **11 pt** body text and reduce stepwise (also allowed: shrink image area, tighten spacing) down to a minimum of **8 pt**. If it still does not fit, return a clear "does not fit" error (HTTP 422 with details), not a second page and not truncated content.
- The validation endpoint reports fit status and the chosen font size, so agents can shorten recipes before saving.
- Embed fonts in the repository/binary; output must be deterministic across machines.

## 12. Image storage

- Define a Rust trait (e.g. `put`, `get`/stream, `exists`, `delete` — delete used only by explicit maintenance tooling) with opaque keys.
- v1 implementation: local filesystem content directory (configurable path).
- Recipes store opaque image references, never filesystem paths or provider URLs.
- Swapping to an S3-compatible backend later must not change domain logic, the API, or stored references.
- Content-addressed or ULID keys; uploaded originals plus derived web/print sizes are acceptable.

## 13. Operations

- Configuration via file + env vars: bind address, database path, content directory, API tokens.
- Database migrations run on startup or via explicit command (document which).
- Backup: a documented command/script that produces a consistent SQLite backup (online backup API or `VACUUM INTO`) together with the content directory; plus a documented restore procedure that has been tested.
- Dev data: a command to copy production data into an isolated development instance.
- Command to rebuild derived data (ingredient links, search index) from stored revisions.
- Deployment: systemd unit (or container) for the VPS, behind an existing reverse proxy with TLS. Health endpoint.
- Structured logging; no secrets in logs.

## 14. Testing and acceptance criteria

Automated tests (must pass in CI):

1. Create → get → update → history: three revisions retrievable, previous ones unchanged.
2. Stale update returns 409 with the current revision ID.
3. Soft delete hides the recipe from the public site and default search; restore brings it back; history is intact.
4. Unknown ingredient is auto-created; adding an alias makes search by the alias find the recipe; similar names are not merged automatically; merge moves links and aliases.
5. Scaling: website and PDF show identical scaled quantities; fractions render naturally; fixed quantities don't scale.
6. Search: combinations of text, ingredient `all`/`any`/`exclude`, and tag filters return correct results.
7. PDF: typical recipe renders as exactly one A4 page at 11 pt; a long recipe shrinks but stays ≥ 8 pt; an oversized recipe yields a fit error; page count asserted in tests.
8. Public site exposes no revisions, no deleted recipes, and no write endpoints; API rejects writes without a valid token.
9. Image upload, attach, replace: old image still served for old revisions.
10. Invalid Cooklang returns a structured error with line/column.
11. Storage trait has a test suite that any backend implementation must pass (run against the filesystem backend).
12. Backup → restore into a fresh instance yields identical recipes and images.

**End-to-end acceptance (the vertical slice):** in a **local agentic coding session**, the agent itself builds and installs the MCP server, registers it with its own harness, and — through MCP tools only — reads the guidelines, creates a recipe with an image, verifies it on the public site, downloads a one-page PDF, performs an update, demonstrates a rejected stale update, and finds the recipe via an ingredient alias. Record the transcript/steps in the repository as a reproducible check.

## 15. Non-goals for v1

- Accounts, login or editing on the public website.
- Displaying revision history publicly.
- Human-facing advanced search UI.
- Meal planning, shopping lists, nutrition, social features, in-app AI chat.
- Remote MCP transport / hosted MCP for cloud clients (later; design the API so it's easy).
- S3/R2 storage backend (trait only).
- A5 cards, two-up or duplex imposition, multi-page recipes.
- Automatic synonym detection or cross-language ingredient matching.
- Unit conversion features.

## 16. Open for the implementer to decide (document choices)

- Exact database schema and current-revision mechanism.
- How Cooklang metadata/front matter in the body is handled (reject vs. strip).
- Cleanest Cooklang convention for ingredient-list-only recipes.
- Template engine, HTMX usage, CSS approach.
- Image size limits and derived sizes.
- Whether ingredient alias resolution happens at write time (with re-link on alias change) or at query time.
