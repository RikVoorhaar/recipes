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
# Temporary token configuration pending #15; replace with a strong secret.
APP__API_TOKENS=owner:replace-with-a-secret
# Select an existing published version; there is no latest tag.
RECIPES_IMAGE_TAG=v1.2.3
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

### Readable book export and import

With an appropriately mounted export directory, run `recipe-app export --dest
/export/book`. The destination parent must exist. The command manages only
paths in its manifest and refuses to overwrite unrelated files. It preserves
`.git` and other unowned files. The versioned tree contains current published
and soft-deleted recipes, their current public images, and the ingredient
registry. It does not contain old revisions, credentials, drafts, private
images, or ingredient history. Keep the existing `scripts/ops.py` procedure
below as a fallback until a fresh-instance restore has been verified and the
VPS cutover completed.

For import, the database path must not exist; the content directory must be
absent or empty, and both parent directories must exist. The import checks
hashes, validates references and rebuilds into temporary storage before
publishing either target. Configure credentials separately after import.

An existing SQLite database is refused even if it appears empty. SQLite can
have an active WAL reader while permitting a separate process to inspect its
tables, so an emptiness check cannot prove that replacing its file is safe;
tokens and other non-book state also make a database non-empty. Stop the app
and select a **new, nonexistent** database filename and an absent or empty
content directory. Verify these paths and their parents first; do not remove or
overwrite an existing database to make the command pass.

For the reference Compose deployment, shell variables and `.env` do **not**
override the paths explicitly set under `environment:`. In the service
directory, create `restore.compose.yml` alongside `docker-compose.yml`:

```yaml
services:
  recipes:
    environment:
      APP__DATABASE_PATH: /data/restore.sqlite3
      APP__CONTENT_DIR: /data/restore-content
    volumes:
      - ./book-export:/export:ro
```

Place the verified readable tree at `./book-export/book`, readable by UID
10001. Ensure `./recipes-data` is writable by UID 10001, and that
`./recipes-data/restore.sqlite3` and its SQLite sidecars do not exist and
`./recipes-data/restore-content` is absent or empty. Stop the existing service
and use **both** files for the import and every subsequent startup:

```sh
docker compose -f docker-compose.yml stop recipes
docker compose -f docker-compose.yml -f restore.compose.yml config
docker compose -f docker-compose.yml -f restore.compose.yml run --rm --no-deps recipes import --src /export/book
docker compose -f docker-compose.yml -f restore.compose.yml up -d recipes
```

Inspect `config` first to verify both paths and the export mount. The override
mount is read-only for restore; use a separately configured writable mount
when exporting. Keep using the override for restarts, or explicitly migrate
the verified settings into the service configuration. Do not start with the
base file alone: it points to the old database and content directory.

The two target paths cannot be published atomically together. The content
directory is populated first and the database is installed last. A process or
machine crash during publication can leave content files without a database:
inspect and clear only that incomplete fresh target before retrying. Export
stages files and rolls back completed replacements on a returned publication
error; if rollback itself fails, the error reports a retained private staging
directory containing `before/` and `plan.json` for manual recovery. Do not
delete it until the old tree is recovered. After correcting the filesystem
problem, compare `plan.json` with the destination: restore remaining files
from `before/` to their matching relative paths, remove only newly created
paths listed in `new_paths` but not `old_paths`, and restore
`before/manifest.json` last. Previously restored files may already be absent
from `before/`; verify the complete tree against the old manifest before
removing the staging directory. For a failed first export there is no old
manifest; remove only the listed new paths after checking them.
Individual file replacements and
the manifest update are not an atomic group: a crash or forced interruption
can still leave an inconsistent tree. Restore the prior managed files and
manifest from a verified Git revision before retrying (or inspect and remove
only partial managed files after an interrupted first export). Run these commands without concurrent filesystem
modification of their target paths; the path checks do not defend against a
hostile process racing them.
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
