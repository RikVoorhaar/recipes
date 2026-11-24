# Stage 4 recipe assets

This directory contains the Stage 4 bundle: `manifest.json` contains ten structured Cooklang recipes (Chicken Soup copied unchanged), `covers/` contains one JPEG per recipe, and `prompts.json` records the exact per-image prompt text.

## Provenance

The ten covers were generated sequentially with OpenRouter Images API `POST /api/v1/images`, model `black-forest-labs/flux-3-image-klein`. Requests contained only `model` and `prompt`. The API key was read privately from the ignored `.env` and is not included here. Covers are distinct JPEGs, resized to at most 1600×1600; no retries or additional generations were made.

## Seed safely

`scripts/seed_stage4.py` is dry-run by default. It requires an explicit target and token file, disables environment proxies, refuses redirects, and only permits localhost by default. The single remote exception is the exact Tailscale preview URL `http://100.81.11.110:38080`, which requires `--allow-preview`; that opt-in does not change dry-run into a write.

Use the locally stored token file; do not paste or print its contents. For the configured preview token:

```sh
TOKEN_FILE="$HOME/.config/recipe-preview.token"
chmod 600 "$TOKEN_FILE"

# Safe, read-only preview.
python3 scripts/seed_stage4.py \
  --base-url http://100.81.11.110:38080 \
  --token-file "$TOKEN_FILE" \
  --allow-preview

# Apply only when publication is explicitly authorized. Use the verified
# existing Chicken Soup ID; never paste or print the token.
python3 scripts/seed_stage4.py \
  --base-url http://100.81.11.110:38080 \
  --token-file "$TOKEN_FILE" \
  --allow-preview \
  --apply \
  --existing-soup-id VERIFIED_CHICKEN_SOUP_ID
```

For an isolated localhost instance, use the token file configured for that instance:

```sh
python3 scripts/seed_stage4.py \
  --base-url http://127.0.0.1:38145 \
  --token-file /secure/path/to/isolated.token
python3 scripts/seed_stage4.py \
  --base-url http://127.0.0.1:38145 \
  --token-file /secure/path/to/isolated.token \
  --apply
```

The user explicitly authorized immediate publication. The parent operator verified all ten recipes on the pinned Tailscale preview through listing and detail responses, including ten distinct working images. A subsequent dry-run skipped all ten. Do not repeat live writes or restart the preview as part of this work. The script rechecks exact titles before each upload, skips existing titles on every retry, and refuses ambiguous title matches.

For new recipes the supported flow is upload → validate with the uploaded `image_ref` → create → attach with the create response's `base_revision` (two immutable revisions). If attachment fails, the script reports the recipe ID, initial revision and image ref plus safe recovery steps; it never retries or overwrites that title. Chicken Soup is created normally on a fresh target. Attaching to an existing soup requires `--existing-soup-id ID`; the script checks the exact source body and empty image before upload, then attaches against the latest revision. Edited soup and any soup with an existing image are no-ops.

## Checks

The isolated copied-layout run seeded ten fresh recipes and attached ten covers. Each recipe had two revisions. A second apply skipped all ten without uploads or revision changes. All ten passed image-aware Cooklang validation at 11 pt with zero parser warnings, loaded as JPEGs, scaled without duplicate ingredient rows, and returned one-page PDFs. Existing/edited soup guards also passed. The Chicken Soup view-only rendering correction now displays each measured ingredient once, preserves preparation notes, and omits the verified ingredient declaration prologue from instructions; its saved Cooklang body remains unchanged. HTTP regressions cover scaled JSON, HTML, PDF output, ambiguous instruction-like lines, and repeated ingredient occurrences. The updated application binary's deployment status is not verified here.
