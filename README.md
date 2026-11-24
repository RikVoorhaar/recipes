# Recipe App

Publish an existing Cooklang recipe through the `recipe-mcp` adapter to the live preview at <http://100.81.11.110:38080>. Do not create or edit a local recipe file for publication.

## OpenCode quickstart

Install [Rust and Cargo with rustup](https://rustup.rs/) on the machine running OpenCode. From this checkout, run:

```sh
cargo install --path recipe-mcp --locked
command -v recipe-mcp
```

Merge this into `~/.config/opencode/opencode.json`, using the absolute path printed above:

```json
{
  "mcp": {
    "servers": {
      "recipes": {
        "type": "local",
        "command": ["/absolute/path/to/recipe-mcp"],
        "environment": {
          "RECIPE_API_BASE_URL": "http://100.81.11.110:38080",
          "RECIPE_API_TOKEN": "{file:~/.config/recipe-preview.token}"
        }
      }
    }
  }
}
```

OpenCode's [file substitution](https://opencode.ai/docs/config#variables) reads the mode-`0600` token file, which must contain only the API **secret** (not `label:secret`). Keep it outside the repo; never put the token in config, commands, prompts, or chat.

**OpenCode on the preview host:** extract the secret from its single-entry env file (no token is printed):

```bash
(
  set +x
  . /tmp/opencode/recipe-preview/app.env || exit 1
  mkdir -p "$HOME/.config"
  install -m 600 /dev/null "$HOME/.config/recipe-preview.token" &&
    printf '%s' "${APP__API_TOKENS#*:}" > "$HOME/.config/recipe-preview.token"
)
```

**OpenCode on another tailnet machine:** get the bearer secret privately from the preview host and save it to `~/.config/recipe-preview.token` on the OpenCode machine. Set the permissions with `chmod 600 ~/.config/recipe-preview.token`.

Config reloads automatically; if needed run `/reload`. In that session, use `/mcps` and confirm `recipes` connects, then call `recipes_get_guidelines` through MCP. If MCP or the call is unavailable, stop and ask—do not create a file or bypass MCP.

Then prompt:

> Read the writing guidelines and publish a chicken soup recipe using the recipes MCP tools. Do not create or edit local files. Verify it on the public site and give me its URL. If MCP is unavailable, stop and tell me.

See the [v1 specification](recipe-app-v1-spec.md), [implementation plan](recipe-app-v1-implementation-plan.md), [operations guide](docs/operations.md), [OpenAPI spec](openapi.yaml), and [live acceptance evidence](docs/stage7-live-acceptance.md). For local development, run `cp config.example.toml config.toml && cargo run -p recipe-app`; health is at <http://127.0.0.1:3000/health>.
