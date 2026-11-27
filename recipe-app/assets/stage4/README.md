# Stage 4 recipe assets

This directory contains the Stage 4 bundle: `manifest.json` contains ten structured Cooklang recipes (Chicken Soup copied unchanged), `covers/` contains one JPEG per recipe, and `prompts.json` records the exact per-image prompt text.

## Provenance

The ten covers were generated sequentially with OpenRouter Images API `POST /api/v1/images`, model `black-forest-labs/flux-3-image-klein`. Requests contained only `model` and `prompt`. The API key was read privately from the ignored `.env` and is not included here. Covers are distinct JPEGs, resized to at most 1600×1600; no retries or additional generations were made.

## Historical seeding

The original one-off REST seeding script has been removed. The bundle remains
as provenance, not as an instruction to publish or overwrite recipes. Recipe
authoring now follows the draft and publication workflow described in the
project README.

## Checks

The isolated copied-layout run seeded ten fresh recipes and attached ten covers. Each recipe had two revisions. A second apply skipped all ten without uploads or revision changes. All ten passed image-aware Cooklang validation at 11 pt with zero parser warnings, loaded as JPEGs, scaled without duplicate ingredient rows, and returned one-page PDFs. Existing/edited soup guards also passed. The Chicken Soup view-only rendering correction now displays each measured ingredient once, preserves preparation notes, and omits the verified ingredient declaration prologue from instructions; its saved Cooklang body remains unchanged. HTTP regressions cover scaled JSON, HTML, PDF output, ambiguous instruction-like lines, and repeated ingredient occurrences. The updated application binary's deployment status is not verified here.
