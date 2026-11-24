# Recipe-writing guidelines

- Use metric quantities with explicit units. Author quantities only in Cooklang.
- Give the recipe a clear base serving count or yield in structured metadata.
- Mark every ingredient with Cooklang syntax wherever it is used; the ingredient list is derived from the body.
- Prefer canonical ingredient names. Search the ingredient list first; use explicit aliases rather than inventing near-duplicates.
- Include temperatures whenever a useful measurement exists: oven temperature and fan/conventional mode; core temperature for meat, fish, baked goods and custards; surface temperature for pans, woks and griddles when it matters; and temperature plus hold time for liquids or starches where relevant. Say what and where to measure (for example, thickest part or pan surface).
- Be concise for an experienced cook. Keep special techniques and surprising steps; omit mundane steps. Everyday dishes may be an ingredient list with one or two key notes.
- Keep the recipe to one printed page. If validation says it does not fit, shorten it rather than splitting it.
- Before saving a long recipe, POST it to `/api/v1/recipes/validate`. Check `print_fit.fits` and `font_size_pt`; if `status` is `does_not_fit`, shorten the recipe and validate again. The validator is a dry run and does not save the recipe or create ingredients.
- A recipe with no instructions is valid: use a sequence of Cooklang ingredient references as the body, for example `@butter{20%g}\n@flour{100%g}\n@milk{200%ml}`.
- Keep title, description, servings, tags and other metadata in the JSON API fields, not Cooklang metadata. Both YAML front matter and legacy `>> key: value` metadata are rejected.
- Source URL and adaptation notes are optional.
- Never invent times, temperatures, or other precision when importing a recipe. Add only temperatures you are confident are correct; proposed temperatures should be clearly identified as suggestions.
