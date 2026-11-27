# Operations

## Compose deployment

`deploy/docker-compose.yml` is the reference for a dedicated infrastructure
service directory. It assumes host-networked Traefik with Docker discovery,
the `websecure` entrypoint and a configured certificate covering
`recipes.rikvoorhaar.com`. It publishes no host ports; Traefik reaches the
container on its dedicated bridge network. The MCP router applies its own rate
and request-body limits. Verify the actual proxy configuration, certificate
coverage and DNS before deploying; this repository does not configure them.

Copy the Compose file to the service directory and create `recipes-data` before
starting it. The image runs as UID/GID **10001:10001**; the directory and any
existing database, SQLite sidecar files and content must be writable by that
identity. For a fresh directory, an operator can use `install -d -m 700 -o
10001 -g 10001 recipes-data`. Do not blindly change ownership of existing data.
File-backed databases use SQLite WAL mode and a five-second busy timeout.
Writers still serialize and may fail if a lock lasts longer than that; the
database directory must allow SQLite to create its `-wal` and `-shm` files.

Create `.env` in that directory, restrict it to the deployment operator
(`chmod 600 .env`), and keep it out of version control:

```dotenv
# Select an existing published version; there is no latest tag.
RECIPES_IMAGE_TAG=v1.2.3
# Optional; enables cover generation.
APP__OPENROUTER_API_KEY=replace-with-provider-key
# Optional; exact trusted browser origins, comma-separated, no trailing slash.
APP__MCP_ALLOWED_ORIGINS=https://recipes.rikvoorhaar.com
```

Remove optional entries if not needed. Remove any old `APP__API_TOKENS`
entry after upgrading: existing database tokens are retained, but configuration
no longer provisions them. Protect the env file and Docker access. Empty origin
configuration rejects requests carrying Origin, while non-browser requests
without Origin remain usable. Do not put secrets in command arguments.
The Compose file configures the public base URL explicitly; it is used for MCP
links, never inferred from request headers. See `config.example.toml` for other
settings. Environment variables override the optional config file.

After reviewing the file and provisioning secrets and storage, use
`docker compose config --quiet`, then `docker compose up -d`. Check
`docker compose ps` and `docker compose logs recipes`, public pages, and
authenticated MCP access. An unauthenticated request to `/mcp` should return
401. The `recipe-app healthcheck` command probes the configured local listener
without opening or modifying the database. Migrations run on startup.

Create the first token after startup from the host (never via MCP):

```sh
docker compose exec -T recipes /app/recipe-app token create --label perplexity
docker compose exec -T recipes /app/recipe-app token list
docker compose exec -T recipes /app/recipe-app token rotate --label perplexity
docker compose exec -T recipes /app/recipe-app token revoke --label perplexity
```

`list` includes stable token IDs. If migrated credentials share a label,
`--label` refuses to select ambiguously; use `token rotate --id ID` or
`token revoke --id ID` to manage each credential without transferring drafts.

Create and rotate print the new `rcp_` secret once; copy it securely to the
client. It cannot be recovered from the database. Never put secrets in command
arguments, environment variables, configuration files, logs or chat. List
shows labels, creation and best-effort last-use timestamps, and status only.
Last-use updates are best effort, attempted at most once per token per minute
per server process, with at most one activity write in flight. They may lag or
be missed during contention. Changes are
effective immediately without restart. Rotation invalidates the old secret and
retains its private drafts and cover candidates. Revocation preserves those
drafts but makes them inaccessible; creating another token does not transfer
them. Existing tokens and drafts migrate automatically, including orphaned
drafts, which remain inaccessible. Back up before upgrading.

Each new request is authenticated against the database, and each message in an
MCP batch is rechecked immediately before dispatch. Revocation or rotation
does not cancel a tool already dispatched: it may finish, including writes,
after the credential changes. If a batch loses authorization between messages,
the server returns 401 and does not dispatch the remaining messages; earlier
messages may already have taken effect.
Publishing a GitHub release tagged `vMAJOR.MINOR.PATCH` runs checks and then
builds and publishes that version tag to GHCR. PR and main CI do not build or
publish an image; the release workflow checks the event's tagged commit and
builds that same commit, even if the Git tag later moves. It does not publish
`latest` or deploy.
Select a real published version with `RECIPES_IMAGE_TAG` before running Compose
and retain the previous version for rollback. Changing the release or version
does not automatically update a running deployment; deployment is manual.
For local image verification use `make container-build` (optionally override
`IMAGE`); this only builds locally.
Before schema-affecting upgrades, make and verify a consistent backup.

For a derived-data rebuild, stop the service and use the same Compose config
and mounted data: `docker compose stop recipes`, then
`docker compose run --rm --no-deps recipes rebuild-derived`, followed by
`docker compose up -d`. Check command exit status before restarting.

## Backup, restore, and development copies

There is no scheduled backup or borg integration in this deployment.
Copying a live SQLite database, especially in WAL mode, is not a consistent
backup. `scripts/ops.py` remains available on the operator's checkout; it uses
SQLite's online backup API and includes the content directory. Stop **all**
writers before backup or dev-copy to keep the database and image files
consistent with one another. Protect backups off-host; they contain private data.
For example, after stopping the Compose service and checking it stopped, run
the existing host script as root (or another identity explicitly authorized to
traverse and read the UID 10001 data). Root can read the recommended mode-700
directory without changing its permissions. Use absolute paths and a trusted
checkout; ensure every parent directory is traversable by the chosen identity.
Create protected, writable backup and restore parent directories in advance.
The final backup and restore paths must not already exist:

```sh
sudo install -d -m 700 -o root -g root /secure/backup /secure/restore
sudo python3 /path/to/trusted/checkout/scripts/ops.py backup \
  --database /path/to/recipes-data/recipes.sqlite3 \
  --content /path/to/recipes-data/content --output /secure/backup/new-directory
sudo python3 /path/to/trusted/checkout/scripts/ops.py restore \
  --backup /secure/backup/new-directory \
  --destination /secure/restore/new-directory
```

Restore verifies checksums and SQLite integrity and refuses an existing
destination. A root-run restore produces root-owned files; before using the
restored directory as `/data`, ensure its database, content and directories are
owned by UID/GID 10001 and writable by that identity, while keeping the data
private. Adjust ownership only on the verified restored copy, not on the
backup or a running service. Verify restored data and permissions before using
it with a stopped service; never merge into live data. Restart with
`docker compose up -d` and check health. `ops.py dev-copy` similarly requires
a nonexistent destination and clears copied API tokens. Readable export/import
(#8) and private Git backup/scheduling (#17) are separate work.

## Verification

Run `make ci` for formatting, all-target Clippy, Rust workspace tests, and
Python operations tests. These use isolated fixtures. The real VPS certificate,
proxy setup, and restoration from its backups require separate operator
verification.
