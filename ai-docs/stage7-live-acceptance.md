# Stage 7 live acceptance evidence

**Current result: passed, 2026-11-24.** The Terra reviewer follow-up below is
the authoritative evidence that the agent itself installed and registered MCP
and performed the public/PDF checks. The earlier run is retained as historical
context; its operator-owned setup and separate-terminal public checks did not,
by themselves, satisfy that evidence requirement.

## Terra reviewer follow-up: agent-owned setup and public checks

This run used one actual OpenCode session throughout both phases:

- Harness: OpenCode 2.3.2; session `ses_df0f71324ffeJMcOUYTw6Vx1rQ`.
- Model: `openai/gpt-6-luna` only.
- The operator provisioned only the fresh local app/SQLite/content sandbox,
  random private API token, and local PNG fixture. The agent used its shell
  tool to build/install `recipe-mcp`, write and validate its own isolated
  OpenCode MCP configuration, and perform all public HTTP/PDF checks.
- No production data, deployment, or sudo was used. No credentials are recorded.

### Agent-owned Phase A: install and registration

The agent's first shell call accidentally included an extra trailing `.` and
failed with Cargo exit code 101:

```sh
docker run --rm -v /home/rik/git/recipes:/work \
  -v /tmp/recipe-stage7-terra.x3fTZh/install:/install \
  -w /work rust:1.92 cargo install --path recipe-mcp --locked --root /install .
```

The agent retried with the valid locked install command; its shell result
reported `Installed package recipe-mcp v0.1.0` and exit code 0:

```sh
docker run --rm -v /home/rik/git/recipes:/work \
  -v /tmp/recipe-stage7-terra.x3fTZh/install:/install \
  -w /work rust:1.92 cargo install --path recipe-mcp --locked --root /install
```

The agent then wrote
`/tmp/recipe-stage7-terra.x3fTZh/harness/config/opencode/opencode.json`
itself. Its first version put `recipes` at the wrong level; in Phase B the
agent corrected it to `mcp.servers.recipes`, validated the JSON, and confirmed
the executable existed and was executable. The MCP server used env
placeholders, not embedded credentials:

```json
{
  "model": "openai/gpt-6-luna",
  "mcp": {
    "servers": {
      "recipes": {
        "type": "local",
        "command": ["/tmp/recipe-stage7-terra.x3fTZh/install/bin/recipe-mcp"],
        "codemode": false,
        "environment": {
          "RECIPE_API_BASE_URL": "{env:RECIPE_API_BASE_URL}",
          "RECIPE_API_TOKEN": "{env:RECIPE_API_TOKEN}"
        }
      }
    }
  }
}
```

After the correction, the agent successfully called `recipes_get_guidelines`
in that same session, demonstrating that its registered stdio server connected.

### Agent-owned Phase B: MCP and public verification

All authoring, image, revision/conflict, and search operations below were MCP
tool calls from the live agent. Public routes were checked later by that same
agent through its shell tool; the operator did not perform those checks.

| Action | Tool result |
| --- | --- |
| `recipes_get_guidelines` | Read writing guidelines before authoring. |
| `recipes_create_recipe` | Created **Terra Gate Lemon Chickpea Salad**, recipe `rcp_b943817dbd864739848a74ec5e5606bf`, initial revision `rev_93e5c3ececfd439292eb866a494cdd79`, 2 servings, tag `stage7-terra`. |
| `recipes_upload_image` / `recipes_attach_image` | Uploaded local fixture and attached `img_4ac3c8b0cd0d4eda92adcd0ace5b809d`; attachment revision `rev_70bf4ee583b24e69bc7c60349557b8df`. |
| `recipes_update_recipe` | Updated description using the current revision; final revision `rev_9a27733bbc824f73a0f20a003c6807c3`. |
| stale `recipes_update_recipe` | **HTTP 409**, `current_revision_id` `rev_9a27733bbc824f73a0f20a003c6807c3`; no retry. |
| `recipes_get_revision` | Retrieved the initial revision, which had no image and an empty description. |
| `recipes_list_ingredients`, `recipes_add_ingredient_alias` | Found canonical chickpeas (`ing_eec68e172aad49c79d3bb5acfd897494`) and added `terra gate alias`. |
| `recipes_search_recipes` | Search by `ingredients=terra gate alias` and `tags=stage7-terra` returned the recipe. |

The agent's shell tool issued the public requests and recorded:

```sh
curl -sS -D /tmp/recipe-stage7-terra.x3fTZh/index.headers \
  -o /tmp/recipe-stage7-terra.x3fTZh/index.html \
  -w 'HTTP %{http_code} content-type %{content_type}\n' \
  http://127.0.0.1:3000/
curl -sS -D /tmp/recipe-stage7-terra.x3fTZh/detail.headers \
  -o /tmp/recipe-stage7-terra.x3fTZh/detail.html \
  -w 'HTTP %{http_code} content-type %{content_type}\n' \
  'http://127.0.0.1:3000/recipes/rcp_b943817dbd864739848a74ec5e5606bf?servings=4'
curl -sS -o /dev/null \
  -w 'HTTP %{http_code}\nContent-Type: %{content_type}\n' \
  http://127.0.0.1:3000/images/img_4ac3c8b0cd0d4eda92adcd0ace5b809d
curl -sS -o /tmp/recipe-stage7-terra.x3fTZh/recipe.pdf \
  -w 'HTTP %{http_code}\nContent-Type: %{content_type}\nDownloaded bytes: %{size_download}\n' \
  'http://127.0.0.1:3000/recipes/rcp_b943817dbd864739848a74ec5e5606bf.pdf?servings=4'
```

The shell-call results showed HTTP 200 for index and detail, the title in the
index, and the updated description, servings 4, image ref, `800 g chickpeas`,
`30 ml olive oil`, and `60 ml lemon juice` in the scaled detail. The image
returned HTTP 200 with `Content-Type: image/jpeg`. The PDF returned HTTP 200 as
`application/pdf` (80,378 bytes).

In a further same-session shell call, the agent repeated the public detail GET
and parsed the form with Python's `HTMLParser`. It matched
`{'type': 'number', 'name': 'servings', 'min': '1', 'max': '1000', 'value': '4'}`
and separately asserted the updated description and image reference; all three
assertions passed with HTTP 200.

The agent used `lopdf` to inspect the downloaded PDF itself. It wrote a small
Rust checker under the temporary sandbox, compiled it with `rustc` inside
`rust:1.92` against the workspace's existing `lopdf` rlib, and ran it against
the downloaded file. The agent's shell output was:

```text
page count: 1
MediaBox: [0, 0, 595.2755737304688, 841.8897705078125]
  (595.2755737304688 × 841.8897705078125 pt)
A4=true
```

One continuation attempt was cut short by shell quoting (`season: command not
found`, exit 127) after creating the recipe; no remaining acceptance actions
were claimed from that attempt. The agent then resumed the same session with a
quoted prompt and completed the MCP and public checks listed above. A
`/work/target/...` artifact lookup also failed from the host shell; the agent
correctly retried using the checkout-relative `target/debug/deps` path before
the successful `lopdf` inspection.

## Earlier run (historical context; not the reviewer evidence)

The earlier run below passed the functional checks, but the operator built and
registered its adapter and performed public/PDF checks in a separate terminal.
It is retained for context only; the agent-owned follow-up above closes that
evidence gap.

This is the separate local-agent release gate
from [plan Stage 7](recipe-app-v1-implementation-plan.md#7-operations-and-release-acceptance)
and [spec §14](recipe-app-v1-spec.md#14-testing-and-acceptance-criteria).
It used no production data or deployment.

## Isolated setup

- Harness: OpenCode 2.3.2, actual agent session `ses_df0fbb5a0ffe3I2XxYwWWnZBms`.
- Model: `openai/gpt-6-luna` (the run identified itself as `gpt-6-luna`).
- App: local `127.0.0.1:3000`, disposable SQLite database and content directory
  under `/tmp/recipe-stage7-live`.
- MCP: `recipe-mcp` built and installed from this checkout at
  `/tmp/recipe-stage7-cargo-root/bin/recipe-mcp`; registered as a local stdio
  server in the isolated OpenCode configuration. The API token was randomly
  generated, private to this temporary run, and is not recorded here.
- A locally generated PNG fixture was supplied to the MCP image-upload tool.

The host did not have Cargo installed, so the quality gates and adapter install
ran in a disposable `rust:1.92` container with the checkout and install root
mounted. `rustfmt` and `clippy` were installed in that container. All commands
completed successfully:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo install --path recipe-mcp --locked --root /install
```

The workspace run included the MCP stdio integration test (`1 passed`) and all
other workspace tests. The app was then started with a fresh temporary DB and
content path and returned `{"status":"ok"}` from `/health`.

## Redacted agent transcript

The agent itself used the registered MCP tools; no API or database substitute
was used for authoring, image work, revision operations, or search.

| MCP tool | Recorded result |
| --- | --- |
| `recipes_get_guidelines` | Read the API-served writing guidance before authoring. |
| `recipes_create_recipe` | Created **Stage 7 Citrus Chickpea Bowl**, 2 servings, `stage7-live` tag, with measured Cooklang ingredients. Recipe `rcp_b79047a400c74f0e90a394ac78ee847b`; initial revision `rev_76d0d9feb6e446bc915079d7e2044a7f`. |
| `recipes_upload_image` | Uploaded the local PNG; opaque image ref `img_3725713d15ee463e8ff5194ab50b6501`. |
| `recipes_attach_image` | Attached the image using the initial base revision; new revision `rev_33e7f50f57fc4d56b8d597969dedf31a`. |
| `recipes_update_recipe` | Changed the description using the current base revision and preserved the image and recipe body; current revision `rev_20f617bd5e414949aad6cc847075b6ea`. |
| `recipes_update_recipe` with stale initial base | Rejected with **HTTP 409**, code `conflict`, message `The recipe changed`, and `current_revision_id` `rev_20f617bd5e414949aad6cc847075b6ea`. The agent did not retry the stale write. |
| `recipes_get_revision` | Retrieved the original revision `rev_76d0d9feb6e446bc915079d7e2044a7f`; it retained the original description and had no image attachment. |
| `recipes_list_ingredients` | Found canonical `chickpeas`, ingredient `ing_77040510ed194a799978020324b45519`. |
| `recipes_add_ingredient_alias` | Added alias `stage7 alias` to that canonical ingredient. |
| `recipes_search_recipes` | Filtered by `ingredients=stage7 alias` and `tags=stage7-live`; returned the created recipe with `matched_ingredients: ["chickpeas"]`. |

## Public HTTP and PDF verification

After the agent returned the recipe ID, a separate terminal used only public
HTTP routes:

```sh
curl -f "http://127.0.0.1:3000/recipes/rcp_b79047a400c74f0e90a394ac78ee847b"
curl -f "http://127.0.0.1:3000/recipes/rcp_b79047a400c74f0e90a394ac78ee847b?servings=4"
curl -f "http://127.0.0.1:3000/recipes/rcp_b79047a400c74f0e90a394ac78ee847b.pdf?servings=4" \
  -o /tmp/recipe-stage7-live/recipe.pdf
```

The public page showed the updated description and image. The image URL returned
`200 OK` as `image/jpeg`. The 4-serving page showed `800 g chickpeas`, `30 ml
olive oil`, `60 ml lemon juice`, `4 g ground cumin`, `4 g salt`, `120 g baby
spinach`, and `100 g feta`, confirming scaling from the saved 2-serving recipe.
The PDF downloaded successfully; `file` reported a one-page PDF, its page tree
reported one page, and its media box was `595.2756 × 841.8898 pt` (A4). Text
inspection of the PDF showed the same 4-serving ingredient quantities.

No token or credential is included in this evidence. The temporary app, harness
configuration/data, credentials, image fixture, and generated PDF are outside
the repository.
