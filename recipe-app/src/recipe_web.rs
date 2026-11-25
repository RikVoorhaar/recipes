//! The cooking view: map authored Cooklang amounts to steps without changing
//! the scaled JSON or print representations.
use crate::{escape_html, format_quantity, method_content, normalize, ScaledRecipe};
use cooklang::{quantity::Quantity, Content, Converter, Ingredient, Item, Modifiers, Recipe};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

#[derive(Default)]
struct Gather {
    amounts: Vec<Quantity>,
    steps: Vec<usize>,
}

impl Gather {
    fn add(&mut self, amount: &Quantity, converter: &Converter) {
        for total in &mut self.amounts {
            if let Ok(sum) = total.try_add(amount, converter) {
                *total = sum;
                return;
            }
        }
        self.amounts.push(amount.clone());
    }
}

fn gather_key(
    index: usize,
    ingredients: &[Ingredient],
    declared: &HashMap<String, Option<usize>>,
) -> Option<usize> {
    let ingredient = &ingredients[index];
    if ingredient.relation.is_intermediate_reference()
        || ingredient.modifiers().contains(Modifiers::HIDDEN)
    {
        return None;
    }
    if ingredient.relation.is_regular_reference() {
        let (parent, _) = ingredient.relation.references_to()?;
        return ingredient.quantity.as_ref().map(|_| {
            let original = &ingredients[parent];
            // A note can describe a different preparation, and an optional
            // portion must not become part of a required total.
            if ingredient.note == original.note
                && ingredient.modifiers().contains(Modifiers::OPT)
                    == original.modifiers().contains(Modifiers::OPT)
                && !original.modifiers().contains(Modifiers::HIDDEN)
            {
                parent
            } else {
                index
            }
        });
    }
    if ingredient.quantity.is_none() && ingredient.note.is_none() {
        if let Some(Some(parent)) = declared.get(&normalize(&ingredient.name)) {
            return Some(*parent);
        }
    }
    Some(index)
}

pub(super) fn render(
    recipe: &Recipe,
    declarations: &HashMap<usize, String>,
    view: &ScaledRecipe,
    converter: &Converter,
) -> (String, String) {
    let mut declared = HashMap::new();
    for &index in declarations.keys() {
        // Duplicate declarations make an unmeasured mention ambiguous.
        declared
            .entry(normalize(&recipe.ingredients[index].name))
            .and_modify(|entry| *entry = None)
            .or_insert(Some(index));
    }
    let keys = (0..recipe.ingredients.len())
        .map(|index| {
            if declarations.contains_key(&index) {
                Some(index)
            } else {
                gather_key(index, &recipe.ingredients, &declared)
            }
        })
        .collect::<Vec<_>>();
    let mut gather = BTreeMap::<usize, Gather>::new();
    for (ingredient, key) in recipe.ingredients.iter().zip(&keys) {
        if let Some(index) = key {
            let row = gather.entry(*index).or_default();
            if let Some(amount) = &ingredient.quantity {
                row.add(amount, converter);
            }
        }
    }

    let mut titles = Vec::new();
    let mut method = String::new();
    let mut list_open = false;
    for ((title, content), parts) in
        method_content(recipe, !declarations.is_empty()).zip(&view.instructions)
    {
        let Content::Step(step) = content else {
            if list_open {
                method.push_str("</ol>");
                list_open = false;
            }
            for part in parts {
                let _ = write!(
                    method,
                    "<p class=\"method-note\">{}</p>",
                    escape_html(part.text())
                );
            }
            continue;
        };
        // A list with no action does not need a pretend step.
        if step.items.iter().all(|item| {
            matches!(item, Item::Ingredient { .. })
                || matches!(item, Item::Text { value } if value.trim().is_empty())
        }) {
            continue;
        }
        let number = titles.len() + 1;
        titles.push(title);
        if !list_open {
            let _ = write!(
                method,
                "<ol class=\"method-list\" start=\"{number}\" role=\"list\">"
            );
            list_open = true;
        }
        let heading_class = if title.is_some() {
            "step-heading step-heading--named"
        } else {
            "step-heading"
        };
        let _ = write!(
            method,
            "<li id=\"step-{number}\" tabindex=\"-1\"><h3 class=\"{heading_class}\"><span class=\"step-number\" aria-hidden=\"true\">{number}</span><span class=\"sr-only\">Step {number}</span>{}</h3><p>",
            title.map(|title| format!("<span class=\"sr-only\">: </span>{}", escape_html(title))).unwrap_or_default()
        );
        for (item, part) in step.items.iter().zip(parts) {
            match item {
                Item::Ingredient { index } => {
                    let ingredient = &recipe.ingredients[*index];
                    if let Some(row) = keys[*index].and_then(|key| gather.get_mut(&key)) {
                        if row.steps.last() != Some(&number) {
                            row.steps.push(number);
                        }
                    }
                    let phrase = match &ingredient.quantity {
                        Some(amount) => format!("{} {}", format_quantity(amount), part.text()),
                        None => part.text().to_owned(),
                    };
                    if ingredient.relation.is_intermediate_reference()
                        || (ingredient.relation.is_regular_reference()
                            && ingredient.quantity.is_none())
                    {
                        method.push_str(&escape_html(&phrase));
                    } else {
                        let _ = write!(
                            method,
                            "<span class=\"ingredient-name\">{}</span>",
                            escape_html(&phrase)
                        );
                    }
                    if let Some(note) = ingredient.note.as_deref().filter(|note| !note.is_empty()) {
                        let _ = write!(method, " ({})", escape_html(note));
                    }
                    if ingredient.modifiers().contains(Modifiers::OPT) {
                        method.push_str(" (optional)");
                    }
                }
                _ => method.push_str(&escape_html(part.text())),
            }
        }
        method.push_str("</p></li>");
    }
    if list_open {
        method.push_str("</ol>");
    }

    let mut ingredients = String::from("<ul class=\"ingredient-list\" role=\"list\">");
    for (key, row) in gather {
        let ingredient = &recipe.ingredients[key];
        let amounts = row
            .amounts
            .iter()
            .map(format_quantity)
            .collect::<Vec<_>>()
            .join(" + ");
        let _ = write!(
            ingredients,
            "<li><span class=\"gather-name\">{}</span> <span class=\"gather-amount\">{}</span>",
            escape_html(&ingredient.display_name()),
            escape_html(&amounts)
        );
        let notes = ingredient
            .note
            .iter()
            .chain(declarations.get(&key))
            .map(String::as_str)
            .filter(|note| !note.is_empty())
            .chain(
                ingredient
                    .modifiers()
                    .contains(Modifiers::OPT)
                    .then_some("optional"),
            )
            .collect::<Vec<_>>()
            .join(" · ");
        if !notes.is_empty() {
            let _ = write!(
                ingredients,
                " <span class=\"gather-note\">{}</span>",
                escape_html(&notes)
            );
        }
        if titles.len() > 1 && !row.steps.is_empty() {
            ingredients.push_str(" <span class=\"ingredient-uses\">Used in ");
            for (index, number) in row.steps.iter().enumerate() {
                if index > 0 {
                    ingredients.push_str(", ");
                }
                let label = match titles[*number - 1] {
                    Some(title) => format!("Step {number}: {title}"),
                    None => format!("Step {number}"),
                };
                let _ = write!(
                    ingredients,
                    "<a href=\"#step-{number}\" aria-label=\"{}\">Step {number}</a>",
                    escape_html(&label)
                );
            }
            ingredients.push_str("</span>");
        }
        ingredients.push_str("</li>");
    }
    ingredients.push_str("</ul>");
    (ingredients, method)
}
