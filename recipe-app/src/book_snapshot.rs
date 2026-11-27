//! Read-only, in-memory representation of the current published book.
//!
//! This is deliberately separate from filesystem publication: callers must
//! validate paths and stage writes before using these bytes as an export.
use crate::{ingredient_names, normalize, storage::ImageStorage};
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};
use std::{collections::BTreeMap, error::Error, io};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(FromRow)]
struct Recipe {
    id: String,
    revision_id: String,
    deleted: i64,
    updated_at: String,
    author: String,
    title: String,
    description: String,
    servings: i64,
    tags: String,
    source_url: Option<String>,
    notes: Option<String>,
    body: String,
    image_ref: Option<String>,
}

#[derive(FromRow, Serialize)]
struct Ingredient {
    id: String,
    name: String,
    normalized_name: String,
    redirect_to: Option<String>,
    deleted: i64,
}

#[derive(FromRow, Serialize)]
struct Alias {
    normalized_alias: String,
    alias: String,
    ingredient_id: String,
}

#[derive(Serialize)]
struct Registry {
    ingredients: Vec<Ingredient>,
    aliases: Vec<Alias>,
}

#[derive(Serialize)]
struct Link {
    ordinal: usize,
    wording: String,
    ingredient: String,
}

#[derive(Serialize)]
struct FrontMatter<'a> {
    id: &'a str,
    title: &'a str,
    description: &'a str,
    servings: i64,
    tags: Vec<String>,
    source_url: &'a Option<String>,
    notes: &'a Option<String>,
    image: Option<String>,
    image_ref: &'a Option<String>,
    updated_at: &'a str,
    author: &'a str,
    ingredient_links: Vec<Link>,
}

/// A deterministic set of relative paths and file bytes, without a manifest.
/// It does not include drafts, tokens, private images, or revision history.
pub struct Snapshot {
    pub files: BTreeMap<String, Vec<u8>>,
    pub recipes: usize,
    pub ingredients: usize,
}

fn invalid(message: &str) -> Box<dyn Error + Send + Sync> {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else {
            dash = true;
        }
    }
    if out.is_empty() {
        out.push_str("recipe");
    }
    out.truncate(out.len().min(80));
    out.trim_end_matches('-').to_owned()
}

fn insert(files: &mut BTreeMap<String, Vec<u8>>, path: String, bytes: Vec<u8>) -> Result<()> {
    if files.insert(path, bytes).is_some() {
        return Err(invalid("export filename collision"));
    }
    Ok(())
}

/// Capture database rows in one SQLite read transaction. Image objects are
/// immutable, so reading referenced objects during that transaction preserves
/// the same published state even while the server accepts writes.
pub async fn capture(pool: &SqlitePool, images: &dyn ImageStorage) -> Result<Snapshot> {
    let mut tx = pool.begin().await?;
    let recipes: Vec<Recipe> = sqlx::query_as(
        "SELECT r.id, r.current_revision_id AS revision_id, r.deleted, \
         v.created_at AS updated_at, v.author_label AS author, v.title, v.description, \
         v.servings, v.tags, v.source_url, v.notes, v.body, v.image_ref \
         FROM recipes r JOIN recipe_revisions v ON v.id=r.current_revision_id \
         AND v.recipe_id=r.id ORDER BY r.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let recipe_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recipes")
        .fetch_one(&mut *tx)
        .await?;
    if recipe_count != recipes.len() as i64 {
        return Err(invalid("recipe has no current revision"));
    }
    let ingredients: Vec<Ingredient> = sqlx::query_as(
        "SELECT id,name,normalized_name,redirect_to,deleted FROM ingredients ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let aliases: Vec<Alias> = sqlx::query_as(
        "SELECT normalized_alias,alias,ingredient_id FROM ingredient_aliases ORDER BY normalized_alias",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut current = BTreeMap::new();
    for ingredient in &ingredients {
        current.insert(
            ingredient.normalized_name.clone(),
            ingredient
                .redirect_to
                .as_ref()
                .unwrap_or(&ingredient.id)
                .clone(),
        );
    }
    for alias in &aliases {
        current
            .entry(alias.normalized_alias.clone())
            .or_insert_with(|| alias.ingredient_id.clone());
    }
    let mut files = BTreeMap::new();
    let ingredient_count = ingredients.len();
    insert(
        &mut files,
        "ingredients.yaml".into(),
        serde_yaml::to_string(&Registry {
            ingredients,
            aliases,
        })?
        .into_bytes(),
    )?;
    for recipe in &recipes {
        if !recipe.id.starts_with("rcp_")
            || !recipe
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || recipe.id.len() < 12
        {
            return Err(invalid("invalid recipe id"));
        }
        let stem = format!("{}--{}", slug(&recipe.title), &recipe.id[..12]);
        let image = if let Some(reference) = &recipe.image_ref {
            if !reference.starts_with("img_")
                || !reference
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                return Err(invalid("invalid public image ref"));
            }
            let private: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM private_image_refs WHERE image_ref=?")
                    .bind(reference)
                    .fetch_optional(&mut *tx)
                    .await?;
            if private.is_some() {
                return Err(invalid("current recipe references a private image"));
            }
            let path = format!("images/{stem}.jpg");
            insert(&mut files, path.clone(), images.get(reference)?)?;
            Some(if recipe.deleted == 0 {
                format!("../{path}")
            } else {
                format!("../../{path}")
            })
        } else {
            None
        };
        let provenance: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT ordinal,normalized_wording,ingredient_id FROM ingredient_resolution_provenance \
             WHERE revision_id=? ORDER BY ordinal",
        )
        .bind(&recipe.revision_id)
        .fetch_all(&mut *tx)
        .await?;
        let words = ingredient_names(&recipe.body);
        if provenance.len() != words.len() {
            return Err(invalid("missing ingredient resolution provenance"));
        }
        let mut links = Vec::new();
        for (ordinal, (word, (saved_ordinal, saved_word, id))) in
            words.into_iter().zip(provenance).enumerate()
        {
            let normalized = normalize(&word);
            if saved_ordinal != ordinal as i64 || normalized != saved_word {
                return Err(invalid("invalid ingredient resolution provenance"));
            }
            if current.get(&normalized) != Some(&id) {
                links.push(Link {
                    ordinal,
                    wording: word,
                    ingredient: id,
                });
            }
        }
        let front = FrontMatter {
            id: &recipe.id,
            title: &recipe.title,
            description: &recipe.description,
            servings: recipe.servings,
            tags: serde_json::from_str(&recipe.tags)?,
            source_url: &recipe.source_url,
            notes: &recipe.notes,
            image,
            image_ref: &recipe.image_ref,
            updated_at: &recipe.updated_at,
            author: &recipe.author,
            ingredient_links: links,
        };
        let yaml = serde_yaml::to_string(&front)?;
        let contents = format!("---\n{}---\n{}", yaml, recipe.body);
        let directory = if recipe.deleted == 0 {
            "recipes"
        } else {
            "recipes/deleted"
        };
        insert(
            &mut files,
            format!("{directory}/{stem}.cook"),
            contents.into_bytes(),
        )?;
    }
    tx.commit().await?;
    Ok(Snapshot {
        files,
        recipes: recipes.len(),
        ingredients: ingredient_count,
    })
}
