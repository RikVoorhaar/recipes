# Recipe App v1 — Staged Implementation Plan

[The v1 spec](recipe-app-v1-spec.md) is authoritative. This plan sequences the work; it does not replace the spec or prescribe a database schema. Each stage should be mergeable on its own. A stage is done when its listed checks pass against a fresh temporary database and content directory, where applicable, and the repository-wide checks remain green.

## Handoff and execution

- Give each implementing agent one stage, its spec sections, and the merged prerequisites. Have it leave the exact test commands, fixtures, and any decisions that the spec delegates to implementers in the repository. Don't make downstream agents rediscover API contracts or parser quirks.
- Merge stages in order by default. After Stage 3, Stages 4 and 5 can run in parallel if their shared recipe/rendering and image lookup contracts are agreed first and route registration is coordinated. Stage 6 needs both. Start the isolated operations work in Stage 7 once the persistence and rebuild interfaces are stable.
- Use integration tests at the API, public HTTP, storage, or stdio boundary as appropriate. Use real SQLite, Cooklang parsing, and Typst for acceptance tests; don't use production data, a deployed VPS, or a live agent harness in CI. Keep the live harness check as a separate release gate.

## 1. Bootable workspace and quality gates

**Spec:** §§2–3, 13. **Depends on:** none.

**Deliverable:** A Rust workspace with the Axum/Tokio app, SQLx migrations, file and environment configuration, structured logging, and a health endpoint. Document how to run it from a clean checkout and when migrations run. Set up CI and a documented pre-commit hook or equivalent, plus reusable helpers for isolated HTTP integration tests.

**Done when:**

- A smoke test starts the app with an empty temporary database and configurable content directory, gets a successful health response, and confirms it can restart against the same database.
- CI and the pre-commit setup run `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`; CI runs `cargo test --workspace`. Extend those gates to any JS, CSS, or Typst tooling when introduced.
- Configuration precedence is documented, and captured startup/request logs do not contain bearer tokens or auth headers.

## 2. Recipe authoring API and immutable history

**Spec:** §§4, 8, 10, 14. **Depends on:** 1.

**Deliverable:** A token-authenticated `/api/v1` with recipe create, current get/list, update, soft delete/restore, and revision list/get/restore. Use a shared recipe service for Cooklang parsing and revision transactions. Start OpenAPI and update it with every subsequent endpoint. Check in the writing guidelines and serve that Markdown through the API.

**Done when:**

- Tokens are configured server-side, hashed at rest, compared in constant time, and attributed by label without exposing the secret. Missing or invalid tokens cannot write. Document the API read-auth policy and use it consistently.
- Create and two successful edits yield three retrievable immutable revisions with one stable recipe ID, timestamps, and author labels. Competing writes based on the same revision cannot both succeed; a stale update returns structured `409` with the current revision ID.
- Delete and restore append revisions; deleted content and history remain available through the API. Restoring a historical revision copies its content into a new revision rather than modifying the old one.
- Invalid Cooklang fails without saving and returns a machine code, human message, and parser diagnostics with line, column, and parser message. Successful saves return parser warnings. Test and document the chosen metadata/front-matter policy and an ingredient-list-only Cooklang fixture.
- Guidelines cover §10, including measured temperatures, concise instructions, and avoiding invented precision. The final dry-run and print-fit workflow is wired up in Stage 5; don't publish an incomplete validator as the final contract.

## 3. Canonical ingredients, tags, and structured search

**Spec:** §§5, 8.1, 13–14. **Depends on:** 2.

**Deliverable:** Integrate ingredient resolution and derived indexes with recipe writes; add ingredient list/search/get with usage counts and linked recipes, alias add/remove, rename, merge, tag counts, structured recipe search, and the derived-data rebuild command. Document alias resolution timing, usage-count semantics, search ordering and pagination.

**Done when:**

- Saving extracts every occurrence, case-folds/trims/collapses whitespace for lookup, creates unknown canonical names, and preserves Cooklang wording for display. Inline Cooklang aliases don't change registry identity. Tests verify that similar names stay distinct.
- An unused alias finds an existing recipe; removing it changes search immediately. Alias collisions with other canonical names are rejected. Merge moves links and aliases to the survivor and retains the source as a soft-deleted or redirecting record. Ingredient and alias changes preserve their history without creating recipe revisions. Test rename, merge, and alias removal; keep an explicit merge fixture for names that already exist as separate canonical records.
- Search fixtures assert exact result sets for text across title, description, notes and body; canonical ingredient IDs and names (including aliases); ingredient and tag `all`/`any`/`exclude`; servings range, image presence, deletion state, and updated-since. Test filters in combination, all three sort modes, stable pagination, and the result fields (ID, title, tags, matched ingredients, snippet). Verify the image-presence filter against real attachments in Stage 4.
- Delete hides a recipe from default search; restore brings it back. Rebuilding derived links and search from stored revisions and the ingredient registry reproduces results and counts without rewriting history or undoing merges. Cover a rebuild after rename, merge, alias removal, and recipe restore.

## 4. Image storage, upload, and revision retention

**Spec:** §§4.3, 8, 12, 14. **Depends on:** 2; coordinate with 3 for search coverage.

**Deliverable:** An opaque-key storage trait with a filesystem backend, authenticated upload and revision-aware attach/detach, and a public image endpoint that never exposes storage paths. Document accepted types, upload limits, and image resize/re-encode policy. Give the renderer a backend-independent way to load image bytes.

**Done when:**

- A reusable storage contract suite passes against the filesystem backend, including read/write round trips, missing keys, and any maintenance-only deletion primitive the trait exposes. Recipes and API documents use opaque references, not paths or provider URLs.
- Tests reject invalid/undecodable and oversized uploads, and confirm valid images are re-encoded/resized according to the documented policy.
- Attach, replace, and detach create new revisions. The previous revision still references the old image and its URL still serves the original stored asset. Delete, restore, and historical restore do not destroy referenced images; no public route exposes recipe revision history. Cover `has-image` with real attachments once Stage 3 is present.

## 5. Shared rendering, public browsing, PDF, and dry-run validation

**Spec:** §§6–8, 10–11, 14. **Depends on:** 2–3; integrate with 4 before closing.

**Deliverable:** One scaled recipe view model used by rendered JSON, server-rendered HTML, and in-process Typst. Build the picture-first public index with title/tag filtering, recipe detail with a no-JS servings form and PDF download, and `GET /recipes/{id}.pdf?servings=N`. Expose API scaled JSON, PDF, and dry-run validation. Check in the Typst template and embedded font assets.

**Done when:**

- Public pages are manually checked at phone and tablet widths with JS disabled. Automated tests cover index/filter, detail, image or placeholder, servings control, and PDF link. Public routes have no editing, writes, or revision history; deleted recipes are absent from index/filter and inaccessible via detail and PDF until restored.
- A shared fixture asserts the same displayed quantities in HTML and the generated PDF, including a natural fraction and a Cooklang fixed quantity that doesn't scale. Viewing another serving count never writes a revision; saving a different base count does. Test that clean formatting doesn't silently change the amount materially.
- PDF tests inspect page count and dimensions: a typical recipe is one A4 page at 11 pt, a longer one auto-fits at 8–11 pt, and oversized content returns structured `422` rather than being clipped or split. Cover picture-present and picture-free output. Check layout, prominent image, and readable temperatures visually; test repeatable output with bundled fonts and no system-font dependency.
- Dry run returns parsed ingredients, canonical resolution preview (including would-be new names), warnings, and print-fit status/font size from the same renderer. Tests verify that dry run changes neither recipe history nor the ingredient registry or indexes. Document how clients handle a non-fitting recipe and show the workflow in the guidelines.

## 6. Local MCP adapter

**Spec:** §§8–10, 14. **Depends on:** 2–5.

**Deliverable:** A standalone stdio MCP binary configured by API base URL and bearer token. It delegates to HTTP and contains no recipe logic. Document build/install and copy-paste client setup for a typical local harness.

**Done when:**

- Stdio integration tests talk to a real temporary HTTP app. Verify guidelines via both resource and `get_guidelines` with identical text, and that create/update descriptions tell agents to read them first.
- The tool catalog covers §9: recipe search and CRUD/restore, revision list/get/restore, upload/attach from a local file, ingredient list/alias/rename/merge, tag list, and validation. Exercise create, local image upload/attach, update, stale update, and alias search over stdio; check that alternate calls forward the documented payloads.
- Update requires a base revision. HTTP conflict and validation details survive the adapter in actionable tool errors. Stdio carries only protocol messages on stdout, and logs and sample configurations contain no real secrets.

## 7. Operations and release acceptance

**Spec:** §§1, 13–15. **Depends on:** 1–6 for sign-off.

**Deliverable:** VPS deployment instructions and a systemd unit or container example behind a TLS reverse proxy, a consistent backup/restore workflow, a production-to-isolated-dev copy command, and a recorded live vertical-slice check.

**Done when:**

- The deployment runbook covers configuration, migrations, persistent paths, token setup, reverse proxy/TLS, health, logs, and invoking the Stage 3 rebuild command. The dev-copy procedure requires an isolated destination and cannot accidentally overwrite the live paths.
- An automated backup/restore test uses the documented commands to restore into a fresh instance and compares current recipes, historical revisions, and referenced image bytes, including an image retained only by history. Document how the SQLite snapshot and content copy are kept consistent and record a tested restore.
- All §14 automated criteria are covered in CI: history/conflicts (Stage 2), ingredient resolution and search (3), retention and storage (4), public visibility/scaling/PDF (5), auth and diagnostics (2), and backup/restore (7).
- In a real local agent session, the agent builds and installs the MCP binary and registers it in its own harness. Using MCP for authoring and search (not direct API or DB shortcuts), it reads the guidelines, creates a recipe, and attaches a local picture. It verifies the recipe and changed servings on the public site, downloads and checks a one-page A4 PDF, updates through MCP, observes a stale-update rejection, retrieves the older revision, and finds the recipe by alias plus tag. Check in reproducible steps or a redacted transcript. This is a release gate, not a substitute for CI.

## Scope guard

Do not add public login or editing, public history, advanced human search, remote MCP, S3 storage, alternate print formats, automatic synonym matching, or unit conversion. Record choices delegated by §16 as part of the stage that makes them; flag any change to spec behavior for approval rather than treating it as an implementation choice.
