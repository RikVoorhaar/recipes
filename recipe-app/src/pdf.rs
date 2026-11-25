use crate::{InstructionPart, ScaledIngredient, ScaledRecipe};
use serde::Serialize;
use std::fmt;
use typst::{
    foundations::{Dict, IntoValue},
    layout::PagedDocument,
};
use typst_as_lib::TypstEngine;

const TEMPLATE: &str = include_str!("../templates/recipe.typ");
const LORA: &[u8] = include_bytes!("../assets/Lora.ttf");
const FIT_SIZES: [u8; 4] = [11, 10, 9, 8];

#[derive(Debug)]
pub(super) enum RenderError {
    DoesNotFit { pages: usize },
    Compile(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DoesNotFit { pages } => write!(
                f,
                "Recipe does not fit on one A4 page at the minimum font size of 8 pt ({pages} pages)"
            ),
            Self::Compile(message) => write!(f, "Recipe PDF compilation failed: {message}"),
        }
    }
}

#[derive(Debug)]
pub(super) struct RenderedPdf {
    pub bytes: Vec<u8>,
    pub font_size_pt: u8,
}

#[derive(Serialize)]
struct PdfRecipe<'a> {
    title: &'a str,
    description: &'a str,
    servings: u32,
    has_image: bool,
    ingredients: Vec<PdfIngredient<'a>>,
    instructions: &'a [Vec<InstructionPart>],
    notes: &'a str,
    source_url: &'a str,
}

#[derive(Serialize)]
struct PdfIngredient<'a> {
    name: &'a str,
    quantity: &'a str,
    note: &'a str,
}

fn data(recipe: &ScaledRecipe) -> Result<String, RenderError> {
    let data = PdfRecipe {
        title: &recipe.title,
        description: &recipe.description,
        servings: recipe.servings,
        has_image: recipe.image.is_some(),
        ingredients: recipe
            .ingredients
            .iter()
            .map(|ingredient: &ScaledIngredient| PdfIngredient {
                name: &ingredient.name,
                quantity: ingredient.quantity.as_deref().unwrap_or(""),
                note: ingredient.note.as_deref().unwrap_or(""),
            })
            .collect(),
        instructions: &recipe.instructions,
        notes: recipe.notes.as_deref().unwrap_or(""),
        source_url: recipe.source_url.as_deref().unwrap_or(""),
    };
    serde_json::to_string(&data).map_err(|error| RenderError::Compile(error.to_string()))
}

/// Compile and render the exact scaled view used by the API and HTML. An
/// omitted image resolver is intentional: image-free recipes do not depend on
/// any external files or system fonts.
pub(super) fn render(
    recipe: &ScaledRecipe,
    image: Option<&[u8]>,
) -> Result<RenderedPdf, RenderError> {
    let recipe_json = data(recipe)?;
    let mut builder = TypstEngine::builder().main_file(TEMPLATE).fonts([LORA]);
    if let Some(bytes) = image {
        builder = builder.with_static_file_resolver([("recipe-image.jpg", bytes)]);
    }
    let engine = builder.build();

    for font_size_pt in FIT_SIZES {
        let mut inputs = Dict::new();
        inputs.insert("recipe".into(), recipe_json.clone().into_value());
        inputs.insert("font-size".into(), f64::from(font_size_pt).into_value());
        let document: PagedDocument = engine
            .compile_with_input(inputs)
            .output
            .map_err(|error| RenderError::Compile(error.to_string()))?;
        let page_count = document.pages.len();
        if page_count == 1 {
            let bytes = typst_pdf::pdf(&document, &Default::default())
                .map_err(|error| RenderError::Compile(format!("{error:?}")))?;
            return Ok(RenderedPdf {
                bytes,
                font_size_pt,
            });
        }
        if font_size_pt == 8 {
            return Err(RenderError::DoesNotFit { pages: page_count });
        }
    }

    unreachable!("the fit sizes always include the minimum size")
}
