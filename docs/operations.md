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

Create `.env` in that directory, restrict it to the deployment operator
(`chmod 600 .env`), and keep it out of version control:

```dotenv
# Temporary token configuration pending #15; replace with a strong secret.
APP__API_TOKENS=owner:replace-with-a-secret
# Optional; enables cover generation.
APP__OPENROUTER_API_KEY=replace-with-provider-key
# Optional; exact trusted browser origins, comma-separated, no trailing slash.
APP__MCP_ALLOWED_ORIGINS=https://recipes.rikvoorhaar.com
```

Remove optional entries if not needed. Tokens are comma-separated
`label:secret` pairs; clients send only the secret. Treat labels as stable
owner identities and protect the env file and Docker access. Empty origin
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
The image is published as `latest` and commit-SHA tags after CI passes; pin a
SHA for reproducible upgrades and retain the previous image for rollback.
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
For example, after stopping the Compose service and checking it stopped:

```sh
python3 scripts/ops.py backup --database /path/to/recipes-data/recipes.sqlite3 \
  --content /path/to/recipes-data/content --output /secure/backup/new-directory
python3 scripts/ops.py restore --backup /secure/backup/new-directory \
  --destination /secure/restore/new-directory
```

Restore verifies checksums and SQLite integrity and refuses an existing
destination. Verify restored data and permissions before using it with a stopped
service; never merge into live data. Restart with `docker compose up -d` and
check health. `ops.py dev-copy` similarly requires a nonexistent destination
and clears copied API tokens. Readable export/import (#8) and private Git
backup/scheduling (#17) are separate work.

## Verification

Run `scripts/check.sh` and `python3 -m unittest discover -s tests -p
'ops*_test.py' -v`. These use isolated fixtures. The real VPS certificate,
proxy setup, and restoration from its backups require separate operator
verification.
