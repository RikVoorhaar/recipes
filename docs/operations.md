# Operations

## Paths, configuration, and lifecycle

The process reads `APP_CONFIG_FILE` (default `config.toml`), then environment
variables; environment overrides file values. The app's relevant settings are
`bind_addr` (default `127.0.0.1:3000`), `database_path` (default
`data/recipes.sqlite3`), `content_dir` (default `data/content`), and
`api_tokens` (array of `label:secret`). `APP__BIND_ADDR`,
`APP__DATABASE_PATH`, and `APP__CONTENT_DIR` override file settings;
`APP__API_TOKENS` is a comma-separated list of `label:secret` values. Keep
tokens in a root-readable environment file, never in source control or command
arguments. The app logs JSON to stderr/stdout through tracing and does not log
authorization headers or token values.

`mcp_allowed_origins` is an array of explicitly trusted browser origins for
`/mcp`; `APP__MCP_ALLOWED_ORIGINS` overrides it with a comma-separated list.
Set the environment variable in the service's environment file or Compose
`environment:` (for example `APP__MCP_ALLOWED_ORIGINS=http://localhost:6274`).
Origins must be exact HTTP(S) origins without paths or trailing slashes. The
empty default rejects every request carrying Origin; requests without Origin
remain usable. Invalid configured origins prevent startup.

Migrations run automatically on each application startup. Startup also runs
the derived-data ensure step. To intentionally rebuild ingredient links and
search-derived state, stop the service and invoke the binary with the service's
config file explicitly:

```sh
sudo systemctl stop recipe-app
sudo -u recipe-app env APP_CONFIG_FILE=/etc/recipe-app/config.toml \
  /opt/recipe-app/recipe-app rebuild-derived
sudo systemctl start recipe-app
```

The rebuild command loads settings from that file; it does not inherit the
systemd unit's environment. For a one-off path override, pass the corresponding
`APP__DATABASE_PATH`, `APP__CONTENT_DIR`, or `APP__BIND_ADDR` in the same
`env` invocation. Environment values override file values; do not override the
database/content paths accidentally, and do not put token secrets on a
command line. `APP__API_TOKENS`, when deliberately supplied in the process
environment, overrides file tokens (the rebuild command does not reconfigure
tokens). This explicit maintenance command is safe to repeat.

## Consistent backup and restore

The backup contains a SQLite online-backup snapshot, a tar archive of the
whole content directory, and SHA-256 checksums. The SQLite backup API captures
a consistent database snapshot. It cannot make a cross-resource transaction
with image files: **stop all application writers first**, so the database
snapshot and image archive describe the same quiescent state. Do not back up
only the database or only the content directory. Output is a new directory
and is never overwritten.

Example production backup (replace the paths to match `/etc/recipe-app/config.toml`):

```sh
sudo systemctl stop recipe-app
sudo -u recipe-app python3 /opt/recipe-app/scripts/ops.py backup \
  --database /var/lib/recipe-app/recipes.sqlite3 \
  --content /var/lib/recipe-app/content \
  --output /var/backups/recipe-app/$(date -u +%Y%m%dT%H%M%SZ)
sudo systemctl start recipe-app
```

Check the stop/backup/start exit status and service health before considering
the backup complete. Keep backup directories off-host and protected: they
contain the complete private recipe database and images. Retention and
off-host replication are operator responsibilities.

Restore always targets a **nonexistent** directory and verifies both checksums
and SQLite integrity before publishing the restored directory:

```sh
sudo systemctl stop recipe-app
sudo -u recipe-app python3 /opt/recipe-app/scripts/ops.py restore \
  --backup /var/backups/recipe-app/20261124T120000Z \
  --destination /var/lib/recipe-app/restore-check
```

To recover service data, verify the restored database and content, then make
the chosen restored paths the configured paths while the service is stopped.
Do not point a running instance at either the source backup or an in-progress
restore. The restore command refuses an existing destination rather than
merging or overwriting it.

## Isolated development copy

`dev-copy` creates an isolated instance directory with `recipes.sqlite3` and
`content/`, using the same verified backup/restore path. The destination must
not exist; it must be separate from both source paths. Never use the live
database or content path as a dev destination. Protect and delete copied data
according to the same privacy policy as production backups.

```sh
sudo systemctl stop recipe-app
sudo -u recipe-app python3 /opt/recipe-app/scripts/ops.py dev-copy \
  --database /var/lib/recipe-app/recipes.sqlite3 \
  --content /var/lib/recipe-app/content \
  --destination /var/lib/recipe-app/dev-copy-$(date -u +%Y%m%dT%H%M%SZ)
sudo systemctl start recipe-app
```

The copied database has its API-token table cleared, so production credentials
are not inherited. Start development only with an explicitly separate config,
bind address, and newly provisioned development token. The command does not
edit configuration or start the copied app.
Stopping production is required for a consistent database-plus-files copy.

## VPS deployment

Build the release binary from a reviewed checkout (`cargo build --release -p
recipe-app`) and install it as `/opt/recipe-app/recipe-app`. Install
`scripts/ops.py` at `/opt/recipe-app/scripts/ops.py`. Create the service user
and persistent directory `/var/lib/recipe-app` out-of-band; set ownership to
that account. Put `config.toml` at `/etc/recipe-app/config.toml`, owned by
root and readable by the service. Configure persistent paths under
`/var/lib/recipe-app`, set a loopback bind address, and provision at least one
`label:secret` API token through the protected environment file. Startup
applies SQLx migrations automatically. On first install, check service logs
and `GET /health` before routing traffic.

Install `deploy/recipe-app.service` as `/etc/systemd/system/recipe-app.service`
and `deploy/nginx-recipe-app.conf.example` as an appropriate site fragment.
Adapt the TLS certificate paths and hostname to the existing reverse proxy;
the example assumes TLS termination at nginx and a loopback-only upstream.
After changes: `systemctl daemon-reload`, enable/start the service, validate
nginx configuration, and reload nginx using the host's normal change process.
Do not expose port 3000 publicly. The proxy must preserve normal request bodies
for image uploads and forward to `/health` for external monitoring.

Inspect service logs with `journalctl -u recipe-app`. Do not enable verbose
request/header logging or include secrets in diagnostics. Before deploying a
schema-affecting upgrade, take and verify a backup. After deployment check
`curl --fail http://127.0.0.1:3000/health`, public pages, and authenticated
API access. Run `rebuild-derived` only when needed; it reconstructs derived
ingredient/search state from recipe revisions and the ingredient registry.

## Verification

The command-level operations test uses temporary SQLite databases and
filesystem fixtures, including an image referenced only by an old revision.
From the repository root, run the same commands shown above:

```sh
python3 scripts/ops.py backup --database "$tmp/live/recipes.sqlite3" \
  --content "$tmp/live/content" --output "$tmp/backup"
python3 scripts/ops.py restore --backup "$tmp/backup" --destination "$tmp/restored"
python3 -m unittest discover -s tests -p 'ops*_test.py' -v
```

The app-level restore acceptance starts the actual application binary against
temporary source and restored instances, creates a recipe with old and
replacement image attachments through the HTTP API, and compares revisions
and served image bytes after restore:

```sh
cargo test -p recipe-app --test ops_restore
```

Both checks use isolated temporary data and have no production-data, VPS, or
live-MCP dependency. The separate real local MCP release gate has been run;
see the [Stage 7 live acceptance evidence](../ai-docs/stage7-live-acceptance.md).
