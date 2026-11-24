# Recipe cover image prompt

Guidelines for generating recipe cover images with an image model (e.g. via OpenRouter). Goal: covers that look like real, natural food photos — not plasticky, oversaturated "AI food" — with a consistent setting across the whole collection.

A prompt is built from two parts: a fixed **house style block** (identical for every recipe) followed by a short **per-recipe block**.

## House style block

Use this text verbatim for every recipe:

> A natural, unretouched photograph of a home-cooked dish, as it might appear in a good home-cooking magazine. Soft daylight from a window on the left, overcast and diffuse, with gentle shadows. True-to-life colours, slightly muted, with no colour grading. Realistic textures: some uneven browning, a few crumbs or drips, sauce that isn't perfectly placed. Food that looks cooked by a skilled home cook, not styled for an advert.
>
> Setting: the dish sits on a red-and-white gingham tablecloth with washed-out, slightly faded cotton and soft creases, on a rustic wooden table. Plain off-white or cream ceramic plate or bowl, simple cutlery. Little else in frame: at most one small, relevant ingredient or a linen napkin, slightly out of focus.
>
> Camera: 45-degree angle, 50mm lens, shallow depth of field, focus on the front of the dish. The whole dish sits within the centre of the frame, with space around it for cropping. No text, labels or watermarks.

## Per-recipe block

Append after the house style block:

> Dish: [name]. [One or two plain sentences on what it looks like finished: main components, colour, how it's served.] Only include ingredients that are in the recipe.

Example:

> Dish: Beef and carrot stew. A thick beef and carrot stew in a shallow bowl, sauce dark brown and glossy, with a slice of crusty bread on the side. Only include ingredients that are in the recipe.

The visual description should be written from the recipe (by the agent/LLM), describing the finished dish — not a copy of the full ingredient list. Image models handle a short description of the finished dish better than a long list of ingredients, and it avoids stray garnishes.

## Notes

- **Phrase everything positively.** FLUX and Seedream on OpenRouter don't take a separate negative prompt, so "not plastic, not oversaturated" tends to work less well than describing the desired look ("muted, natural, unretouched").
- **Avoid hype words** such as "vibrant", "mouthwatering", "delicious", "hyperrealistic", "8K". They push models towards the oversaturated, glossy look.
- **Keep the setting fixed.** Don't randomly alternate between backgrounds; inconsistent backgrounds make the collection look patchy. If variety is wanted, tie it deterministically to the recipe category, e.g.:
  - gingham tablecloth for mains and pasta
  - bare rustic wood for bread and baking
- **Keep the gingham faded.** Bright red checks pull the whole image towards high saturation and can look kitschy.
- **Reference image for consistency (optional).** Pass one approved cover as a reference image ("match the lighting, tablecloth and plate style of the reference"). Check that the chosen model accepts image input on OpenRouter first.
- **If results still look too glossy,** try adding "photographed on film, subtle grain".

## Evaluating models / prompt changes

Generate the same ~10 test recipes per model or prompt variant, including visually difficult dishes (a stew, a layered bake, bread with a cut surface, a salad with several specified ingredients, something with a sauce). Compare side by side at thumbnail size and at the crop used on the recipe page, judging:

- ingredient fidelity (nothing absent from the recipe)
- plausible, non-plasticky textures and natural colour
- consistency of background, lighting and tableware across the set
