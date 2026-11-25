# Draft-first MCP publishing and cover generation

Two phases. For each: one Luna implementation agent, then the same Terra reviewer. Send findings back to Luna and return to Terra until approved. Run the checks and commit only the files for that phase on `main`. Do not stage unrelated working-tree changes. Phase 2 starts after Phase 1 is committed.

## Phase 1 — Make drafts the MCP write path

**Deliverables**

- An authenticated, persisted draft store. A draft can be incomplete and can be resumed by its owner (the same recipe API token identity) in a later session. Drafts never appear in public pages, published recipe history, search, tag counts, or the ingredient directory.
- Five MCP tools: `save_draft`, `get_draft`, `list_drafts`, `discard_draft`, and `publish_draft`. `save_draft` starts or revises a draft; for edits it can start from the current published recipe and records that recipe’s base revision. Existing-draft writes, discard, and publish require the expected draft revision. Publishing an edit also requires the recorded published base still to be current.
- `publish_draft` validates the current draft and atomically creates the new recipe or updates the existing recipe and consumes the draft. Preserve an existing cover on a text-only edit. Failure must leave the draft and the published recipe unchanged.
- Remove `create_recipe` and `update_recipe` from MCP discovery **and dispatch**, with no compatibility aliases. Existing authenticated HTTP write routes are distinct from the MCP interface; keep them for repository seed and operations workflows, not as an alternate agent workflow.
- Advertise the primary workflow in MCP instructions: read `get_guidelines`, save, read back and validate, then explicitly publish. Update the short README and OpenAPI. Cover generation is not required in this phase.

**Definition of done**

- A real stdio MCP test discovers the five tools and proves the two removed tools cannot be called. It saves an incomplete draft, reads it back from a new MCP process, completes it, and publishes explicitly.
- HTTP tests cover owner isolation, no public/index side effects before publish, discard, invalid draft retention, stale draft revision, competing publishes, and a stale edit that cannot overwrite newer published work. Publishing a valid edit keeps the recipe ID and immutable published history.
- Formatting, workspace tests, and Clippy pass. Tests operate on isolated data.

## Phase 2 — Generate covers through MCP

**Deliverables**

- Add `generate_cover` with two explicit target forms. For a **draft**, generate one private candidate and return an inspectable preview plus an opaque reference; generation never publishes. `publish_draft` accepts a selected candidate belonging to that draft and publishes it together with the recipe, or publishes without a candidate. Do not require a cover, even if generation failed.
- For a **published recipe**, generate and attach in one call. Require the expected published revision and explicit opt-in to replace an existing cover. Reject stale or unapproved requests before sending a paid request, and check the revision again when attaching. Never remove the existing cover on provider failure or conflict.
- Implement generation as a server-side library call behind the MCP tool, not a shell script or a series of instructions for the agent. The agent supplies the target and a short description of the finished dish. It does not choose the provider, model, or house prompt.
- Keep the OpenRouter key private on the recipe server. Give the operator server settings for the model (default `black-forest-labs/flux-3-image-klein`) and the house prompt (a configurable Markdown file, with a built-in default based on the checked-in [house style](recipe-cover-image-prompt.md)). The library combines the configured prompt and dish description, then calls the configured model. Never send the entire draft, recipe credentials, or internal metadata to the provider. No automatic paid retries. Keep raw provider errors and secrets out of logs and tool output.
- Make one cover attempt the default in MCP authoring instructions for new recipes unless the user opts out. The agent inspects the result, then explicitly publishes. On generation failure, report it and proceed with explicit publication without a cover when the user asked to publish. Update MCP instructions, README, and OpenAPI.

**Definition of done**

- All automated tests (unit, integration, end-to-end MCP, and CI) must mock the image provider or use a local fake. They must not contact OpenRouter or any external image-generation API, require a real provider key, or incur charges—even if a real key is present in the environment. The fake verifies the request shape, built-in prompt and model defaults, operator-supplied prompt and model overrides, and max one request per call. Test success, provider failure, malformed or oversized output, missing or invalid configuration, revision conflict, and replacement safeguards.
- An end-to-end MCP test generates and inspects a draft candidate without making the draft or cover public, publishes with the chosen cover, and publishes without one after failure. Other tokens cannot read or select a private draft candidate. A persisted candidate is never trusted for a different draft or a stale draft revision.
- A published-recipe test verifies successful attach/replacement, conflict protection, and unchanged cover on failure. Existing upload/attach/detach tools still work.
- Formatting, workspace tests, and Clippy pass. Verify the server and MCP adapter together on isolated data before any preview rollout; do not log or commit credentials. Any live provider check must be a separate, explicitly authorized manual check, never part of the automated test suite.

## Authorized final smoke test — one real recipe

After both phases are reviewed and committed, deploy the updated server to the existing Tailscale preview database and install/reload the updated MCP adapter. Start a **fresh OpenCode session** and drive the workflow using the actual `recipes` MCP, not direct HTTP or SQL: read the guidelines, save and read back one new draft, validate it, generate and inspect one cover, then explicitly publish. Verify the public recipe URL and cover load and confirm the published draft is no longer open. Report the URL and leave the recipe in the real preview database.

Rehearse the migrations on a copy of the preview first. Quiesce writers for a private database-and-content backup at cutover. An older binary may not be able to open the migrated database; if startup fails, recover from the verified backup at a separate path and retain the migrated files. Do not restore a pre-smoke backup over successful publication.

The user authorized **one paid cover request** for this manual check. Do not retry a failed provider call or count a publish without a cover as a passed smoke test. Report any failure and keep the draft available for inspection. Automated tests remain fully mocked.
