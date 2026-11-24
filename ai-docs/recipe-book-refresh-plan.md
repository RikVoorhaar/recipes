# Recipe book refresh

Work on local `main`. Each stage gets one Luna implementation pass at a time and a Terra review. Send each correction to Luna, then return to the same Terra reviewer until approved. Check the diff, then commit that stage before advancing. Do not push. Keep `.env`, tokens, and live data out of Git.

## 1. Organize working documentation

Move the v1 spec, implementation plan, Stage 7 evidence and cover prompt into `ai-docs/`, fixing their links. Preserve the spec and prompt text. Keep entry-point, operations, runtime guidelines, and asset documentation where readers or code expect them. Add a secret ignore rule if missing, without staging anyone else's unrelated work.

**Done:** move/links verified; the short README still points to the important documents; `.env` is ignored and not tracked.

## 2. Repair public browsing and add the ingredient directory

Fix the `/recipes?tag=soup` and `/?q=chicken&tag=` regressions. Keep `/tags/:tag` and use an actual listing link for tags. Add clear empty states and filter reset. Add a public `/ingredients` index of canonical ingredients with usage counts and links to the recipes that use them; omit deleted recipes from usage. Keep existing APIs intact. Give both public indexes discoverable navigation.

**Done:** HTTP regression tests reproduce both failures before the fix and pass afterward; they cover tag/title combinations, blank filters, no results, escaped data and browsing recipes from an ingredient. Existing browsing/search/scaling tests pass.

## 3. Redesign the public app

Aim for a warm, photo-led cookbook: restrained colors, editorial typography, consistent cover crops, clear search, generous whitespace and a recipe page laid out for cooking. Move inline CSS out of handlers. Use one shared stylesheet with CSS custom properties for theme tokens (colors, typography, spacing and geometry), reusable layout patterns, and semantic rules for cards, controls, tags, notices and recipe sections. Share the page shell and repeated markup using the existing stack or a conventional server-side template library if justified; no frontend framework or bespoke templating engine. Document the customization seam. Keep ingredient quantities, instructions, serving scale, PDFs and keyboard use intact.

**Done:** check the recipe list, filtered and empty states, ingredient directory, and recipe detail at desktop and mobile widths; no sideways scroll; visible keyboard focus; proper heading and form labels; test CSS delivery, escaping and core links/actions. Inspect actual rendered pages and get Terra's visual review.

## 4. Make and publish ten illustrated recipes

Use the existing chicken soup as one of ten when suitable; add nine varied, usable recipes, including a stew, layered bake, bread, salad, and a sauced dish. Preserve the existing soup body when attaching a cover. Generate ten distinct covers with OpenRouter `black-forest-labs/flux-3-image-klein`, using the verbatim house block and faithful dish-specific descriptions. Read the git-ignored `.env` without printing the key. Compress covers to suit the app's upload limits. Keep a repeatable, opt-in seed workflow and reviewed source assets with the repo; seed only the local Tailscale preview through the supported write interface. Do not perform a bulk wipe or overwrite unrelated recipes.

**Done:** ten different recipes and ten working covers on the preview; quantities and steps make culinary sense; sample photos at card/detail sizes and validate all ten for scaling, image loads and one-page PDFs. Repeat seeding does not duplicate records or overwrite modified recipes. No keys or live database files are committed.

## 4b. Simplify the cookbook design with the full collection

Keep the design preview's typography, readable spacing, and regular photo grid. Remove the invented brand, promotional headings, repeated card kickers, taglines, and decorative footer copy. Use straightforward labels such as “Recipes” and “Ingredients.” Let the food photography provide the visual character. Put the recipe name near the top on mobile; serve controls and PDF shouldn't compete with the content.

**Done:** compare two refinement passes with all ten recipes and covers in an isolated preview. Inspect the populated grid, filtered and empty states, ingredients, and recipe detail at desktop and mobile widths. Pick the calmer layout that gets readers to the recipes sooner. Preserve keyboard focus, labels, URLs, and the shared theme stylesheet. Get Terra approval and commit before live deployment.

## 5. Deploy the approved build to the existing preview

Smoke test a separate instance or isolated data first. Replace only the preview process, preserving its bind, port, credentials, database and content. Record the checks and rollback procedure without secrets.

**Done:** preview health is green on the same Tailscale URL; the two exact reported URLs show Chicken Soup; `/ingredients` is navigable; ten photographed recipes, responsive styling and PDF/serving links work live. Run applicable workspace checks; commit operational evidence.
