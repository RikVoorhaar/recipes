#import sys: inputs

#let recipe = json.decode(inputs.recipe)
#let font-size = inputs.font-size
#let image-height = if font-size >= 10 { 48mm } else { 36mm }
#let tightness = if font-size >= 10 { 1 } else { 0.82 }
#let ingredient-name(name) = text(weight: "semibold", fill: rgb("#722f37"), name)

#set page(paper: "a4", margin: (x: 13mm, y: 12mm))
#set text(font: "Lora", size: font-size * 1pt, lang: "en", fill: rgb("#25251f"))
#set par(leading: font-size * 0.28pt, spacing: font-size * 0.35pt)
#set list(marker: "•", indent: 1.1em, body-indent: 0.35em, spacing: font-size * 0.18pt)
#set enum(indent: 1.3em, body-indent: 0.25em, spacing: font-size * 0.28pt)

#if recipe.has_image [
  #image("recipe-image.jpg", width: 100%, height: image-height, fit: "cover")
  #v(4mm * tightness)
]

#text(size: font-size * 1.8pt, weight: "bold", recipe.title)
#if recipe.description != "" [
  #v(1mm)
  #text(size: font-size * 0.92pt, fill: rgb("#55564f"), recipe.description)
]
#v(2.5mm * tightness)
#text(weight: "bold", "Serves " + str(recipe.servings))

#v(3mm * tightness)
#grid(
  columns: (0.78fr, 1.22fr),
  gutter: 8mm,
  [
    #text(size: font-size * 1.12pt, weight: "bold", "Ingredients")
    #v(1.5mm)
    #for ingredient in recipe.ingredients [
      - #if ingredient.quantity != "" [
          #text(weight: "bold", ingredient.quantity + " ")
        ]
        #ingredient-name(ingredient.name)
        #if ingredient.note != "" [
          #text(fill: rgb("#62635c"), " (" + ingredient.note + ")")
        ]
    ]
  ],
  [
    #text(size: font-size * 1.12pt, weight: "bold", "Method")
    #v(1.5mm)
    #for step in recipe.instructions [
      + #step.map(part => if part.kind == "ingredient" {
          ingredient-name(part.text)
        } else {
          text(part.text)
        }).join()
    ]
  ],
)

#if recipe.notes != "" [
  #v(3mm * tightness)
  #text(weight: "bold", "Notes  ")
  #recipe.notes
]
#if recipe.source_url != "" [
  #v(2mm)
  #text(size: font-size * 0.72pt, fill: rgb("#62635c"), recipe.source_url)
]
