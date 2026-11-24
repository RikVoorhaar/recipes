# Stage 5 preview deployment evidence

Date: 2026-11-24

## Release

- Approved checkout: `48b5a9e` (Stage 4b), following Stage 4 `f6f9c8d`.
- Built the exact checkout with:
  `PATH="$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR="$HOME/.cache/recipe-book-refresh-target" cargo build --locked -p recipe-app`
- The build succeeded. The approved binary was copied to
  `/tmp/opencode/recipe-preview/releases/20261124T215630Z-pUolM9Dv/recipe-app`
  (SHA-256 `e158c15bb4632c532b5376eadcc47fd5cec2ae8c8369ea4f49cad77b87bf3469`).
- A private binary-only rollback copy of the previous executable is preserved at
  `/tmp/opencode/recipe-preview/releases/rollback-previous-11ad8962/recipe-app`.
  It is byte-for-byte identical to the prior `/home/rik/git/recipes/target/debug/recipe-app`
  (SHA-256 `11ad896259aef2612007d90e77fc7b5b0b2499e09ae1ae70eef1b869a4550c54`).
  Its directory and executable are mode `0500`, preventing accidental writes;
  the root-owned source was not changed. ELF diagnostics identify x86-64 PIE
  Build ID `f99226a31a865f65ca4b403bfe0d733fe0562cf3`.
- Replaced only the verified preview process on `100.81.11.110:38080`.
  Current PID: `2140774`; health endpoint returned HTTP 200 with
  `{"status":"ok"}`.
  The separate listeners on `127.0.0.1:38081` and `127.0.0.1:38127` were not
  changed.
- Preserved the existing protected environment file and the bind, database,
  content paths, and credentials. No credentials or live data were added to Git.
- There are no database migration changes in the approved release.

## Backup and data preservation

- Quiesced the preview writer, then used `scripts/ops.py backup` before starting
  the candidate. Private backup:
  `/tmp/opencode/recipe-preview/backups/20261124T215630Z` (directory mode `0700`).
- Read-only comparison against that backup passed: the recipes, revisions,
  ingredients, aliases, and ingredient occurrences match; all ten content
  filenames and SHA-256 values match.
- The active recipe count remains ten. Chicken Soup remains
  `rcp_a202c36397494083ac349919672dd94a`; its revision, serving count, body,
  and image reference match the pre-deployment backup. No recipe data was
  modified by deployment.

## Live verification

Ran:

```sh
python3 /tmp/opencode/recipe-refresh-smoke.py \
  http://100.81.11.110:38080/ recipe-app/assets/stage4/manifest.json
```

Passed both reported routes, `/ingredients`, and the manifest checks for all
ten recipes: distinct working cover images, serving-scaled detail pages, and
one-page A4 PDFs. `/recipes?tag=soup` shows the existing Chicken Soup ID above;
`/?q=chicken&tag=` shows that same soup plus Chicken Piccata. The health check
and CSS delivery also passed. Requests were made directly to the preview's
Tailscale-bound listener; the independent candidate had already passed the
reported isolated browser checks at 320, 390, and 1200 px, including keyboard
use and no horizontal overflow.

`./scripts/check.sh` passed formatting, all-target Clippy, workspace tests
(including 14 `recipe_api` tests), and MCP stdio checks. All six Python ops
tests passed.

Actual live Firefox verification at 320, 390, and 1200 px passed: all ten recipe
cards and images rendered; there was no horizontal overflow or unlabeled field;
Tab keyboard navigation showed a visible 3 px focus indicator. The live
all-ten HTTP smoke also passed for serving-scaled pages and one-page A4 PDFs.

## Rollback

The previous binary-only rollback copy is independent of the cutover helper and
the mutable, root-owned build output. For a UX regression after candidate
health succeeds, the following manual procedure verifies the exact current
candidate PID and executable before sending it `TERM`, then starts only the
verified rollback copy. It never restores or modifies the database or content.
The protected env file supplies the API token; no token is put in arguments or
logged. The working directory and all application paths/settings are explicit.

```bash
set -euo pipefail
ROOT=/tmp/opencode/recipe-preview
PID_FILE=$ROOT/server.pid
CURRENT_EXE=$ROOT/releases/20261124T215630Z-pUolM9Dv/recipe-app
ROLLBACK_EXE=$ROOT/releases/rollback-previous-11ad8962/recipe-app
ENV_FILE=$ROOT/app.env
DB=$ROOT/recipes.sqlite3
CONTENT=$ROOT/content
WORKDIR=/home/rik/git/recipes

pid=$(<"$PID_FILE")
[[ "$pid" =~ ^[0-9]+$ ]]
[[ "$(readlink -f "/proc/$pid/exe")" == "$CURRENT_EXE" ]]
[[ "$(tr '\0' ' ' < "/proc/$pid/cmdline")" == "$CURRENT_EXE " ]]
kill -TERM "$pid"
for _ in {1..150}; do
  if [[ ! -e "/proc/$pid/exe" ]] ||
     [[ "$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)" != "$CURRENT_EXE" ]]; then
    break
  fi
  sleep 0.1
done
if [[ -e "/proc/$pid/exe" ]] &&
   [[ "$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)" == "$CURRENT_EXE" ]]; then
  echo "Candidate did not exit; refusing to start a second process." >&2
  exit 1
fi

nohup bash -c '
  set +x
  set -a
  source "$1"
  set +a
  export APP__BIND_ADDR=100.81.11.110:38080
  export APP__DATABASE_PATH="$3"
  export APP__CONTENT_DIR="$4"
  cd "$5"
  exec "$2"
' recipe-preview-rollback "$ENV_FILE" "$ROLLBACK_EXE" "$DB" "$CONTENT" "$WORKDIR" \
  </dev/null >>"$ROOT/server.log" 2>&1 &
new_pid=$!
printf '%s\n' "$new_pid" >"$PID_FILE"

for _ in {1..100}; do
  if [[ -e "/proc/$new_pid/exe" ]] &&
     [[ "$(readlink -f "/proc/$new_pid/exe" 2>/dev/null || true)" == "$ROLLBACK_EXE" ]] &&
     [[ "$(tr '\0' ' ' < "/proc/$new_pid/cmdline" 2>/dev/null || true)" == "$ROLLBACK_EXE " ]] &&
     curl --fail --silent http://100.81.11.110:38080/health | grep -q '"status":"ok"'; then
    echo "Previous binary is healthy at http://100.81.11.110:38080 (PID $new_pid)."
    exit 0
  fi
  sleep 0.2
done
echo "Rollback health check failed; inspect $ROOT/server.log." >&2
exit 1
```
