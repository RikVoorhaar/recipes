# Recipe-writing guidelines

- Use metric quantities and put them only in Cooklang. Prefer weight in grams whenever it is practical, especially for salt, spices, and other dry ingredients. Specify measured salt in grams, never in teaspoons or tablespoons.
- When volume is appropriate, use milliliters (`ml`), not teaspoons or tablespoons. For larger amounts of water, milliliters or liters (`l`) are both fine. Use counts where they are natural, such as two eggs.
- Give the recipe a clear base serving count or yield in structured metadata.
- Mark ingredients with Cooklang syntax when they appear in the method so readers can spot them quickly. The ingredient list is derived from the body; do not repeat a quantity just to mark a later mention.
- Give each meaningful step a short action heading with a Cooklang section (`== Toast the crumbs ==`). Keep actions that need to happen together in one step. Identify when work can overlap and name any prepared component you refer back to.
- Put the amount used at each addition: `@butter{20%g}` in the first step and `@&butter{10%g}` in the finishing step gives 30 g to gather. Use `@&butter{}` for a bare reference, not a new portion. Do not guess at missing allocations; keep “to taste,” optional uses, and clearly labeled alternatives explicit.
- Put prerequisites and irreversible decisions before the action (for example, reserve cooking water before draining). Mention consequential timing or equipment early. Give an observable stopping cue alongside relevant time and heat; don't invent precision or rely on color alone.
- Prefer canonical ingredient names. Search the ingredient list first; use explicit aliases rather than inventing near-duplicates.
- Include temperatures whenever a useful measurement exists: oven temperature and fan/conventional mode; core temperature for meat, fish, baked goods and custards; surface temperature for pans, woks and griddles when it matters; and temperature plus hold time for liquids or starches where relevant. Say what and where to measure (for example, thickest part or pan surface).
- Be concise for an experienced cook. Keep special techniques and surprising steps; omit mundane steps. Everyday dishes may be an ingredient list with one or two key notes.
- Keep the recipe to one printed page. If validation says it does not fit, shorten it rather than splitting it.
- Before saving a long recipe, POST it to `/api/v1/recipes/validate`. Check `print_fit.fits` and `font_size_pt`; if `status` is `does_not_fit`, shorten the recipe and validate again. The validator is a dry run and does not save the recipe or create ingredients.
- A recipe with no instructions is valid: use a sequence of Cooklang ingredient references as the body, for example `@butter{20%g}\n@flour{100%g}\n@milk{200%ml}`.
- Keep title, description, servings, tags and other metadata in the JSON API fields, not Cooklang metadata. Both YAML front matter and legacy `>> key: value` metadata are rejected.
- Source URL and adaptation notes are optional.
- Never invent times, temperatures, or other precision when importing a recipe. Add only temperatures you are confident are correct; proposed temperatures should be clearly identified as suggestions.
