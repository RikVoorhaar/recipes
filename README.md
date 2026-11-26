# Recipe App

## OpenCode quickstart

The MCP endpoint uses Streamable HTTP; the stdio adapter has been removed.

Merge this into `~/.config/opencode/opencode.json`:

```json
{
  "mcp": {
    "servers": {
      "recipes": {
        "type": "remote",
        "url": "https://recipes.rikvoorhaar.com/mcp",
        "oauth": false,
        "headers": { "Authorization": "Bearer {file:~/.config/recipes.token}" }
      }
    }
  }
}
```

Save your bearer **secret** (not `label:secret`) to `~/.config/recipes.token` on the OpenCode machine and set permissions with `chmod 600 ~/.config/recipes.token`. OpenCode reads it through [file substitution](https://opencode.ai/docs/config#variables). Keep it outside the repo; never put the secret in config, commands, prompts, or chat.

For browser-based MCP clients, configure trusted origins explicitly using
`mcp_allowed_origins = ["https://inspector.example"]` in `config.toml`, or
`APP__MCP_ALLOWED_ORIGINS=https://inspector.example,http://localhost:6274`
in the server environment (including an environment file or Compose
`environment:`). Use exact origins without paths or trailing slashes. The
default is empty: requests without Origin are accepted, but all browser origins
are rejected until configured. Never derive this allowlist from Host.

Config reloads automatically; if needed run `/reload`. In that session, use `/mcps` and confirm `recipes` connects, then call `recipes_get_guidelines` through MCP. If MCP or the call is unavailable, stop and ask—do not create a file or bypass MCP.

Through MCP, first read `get_guidelines`, save the recipe as a draft, read it
back and validate it, and only then call `publish_draft` when publication is
explicitly requested. Drafts are private to the token owner and persist across
MCP sessions.

### Cover generation (server configuration)

Cover generation is server-side and requires `APP__OPENROUTER_API_KEY`. The
operator can set `APP__COVER_MODEL` (default
`black-forest-labs/flux-3-image-klein`) and `APP__COVER_PROMPT_FILE` (a Markdown
file containing the house prompt; when omitted, the built-in prompt matches
the checked-in house style in `ai-docs/recipe-cover-image-prompt.md`). Set these
only in the recipe server environment. Never configure the provider key in the
MCP client. Generation sends only the finished-dish description and house
prompt to OpenRouter.

Then prompt:

> Read the writing guidelines, draft a chicken soup recipe, read it back and validate it, then explicitly publish it using the recipes MCP tools. Do not create or edit local files. Verify it on the public site and give me its URL. If MCP is unavailable, stop and tell me.

See the [v1 specification](ai-docs/recipe-app-v1-spec.md), [implementation plan](ai-docs/recipe-app-v1-implementation-plan.md), [cover prompt](ai-docs/recipe-cover-image-prompt.md), [operations guide](docs/operations.md), [OpenAPI spec](openapi.yaml), and [live acceptance evidence](ai-docs/stage7-live-acceptance.md). For local development, run `cp config.example.toml config.toml && cargo run -p recipe-app`; health is at <http://127.0.0.1:3000/health>.
