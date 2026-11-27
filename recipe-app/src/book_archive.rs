//! Versioned readable-book tree management.
use crate::book_snapshot::Snapshot;
use crate::{database, ingredient_names, normalize, storage::FileImageStorage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs, io,
    io::Write,
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub app_version: String,
    pub exported_at: String,
    pub recipes: usize,
    pub ingredients: usize,
    pub files: BTreeMap<String, String>,
}

fn invalid(message: &str) -> Box<dyn Error + Send + Sync> {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

fn checksum(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn managed(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    let valid_file = |s: &str, ext: &str| {
        s.ends_with(ext)
            && s.contains("--rcp_")
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    };
    match parts.as_slice() {
        ["ingredients.yaml"] => true,
        ["recipes", file] | ["recipes", "deleted", file] => valid_file(file, ".cook"),
        ["images", file] => valid_file(file, ".jpg"),
        _ => false,
    }
}

/// Refuse symlinks and special files in every existing component. Missing
/// suffixes are permitted; callers must still check expected file types.
pub fn safe_path(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => return Err(invalid("parent path components are not allowed")),
            Component::CurDir => continue,
            _ => current.push(component.as_os_str()),
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(invalid("symlink in path")),
            Ok(meta) if !meta.is_dir() && !meta.is_file() => {
                return Err(invalid("special file in path"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Read-only storage adapter which rejects symlinks in referenced objects.
pub struct ExportImages(pub PathBuf);

impl crate::storage::ImageStorage for ExportImages {
    fn get(&self, key: &str) -> io::Result<Vec<u8>> {
        if !key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid image key",
            ));
        }
        let path = self.0.join(key);
        safe_path(&path).map_err(io::Error::other)?;
        if !fs::symlink_metadata(&path)?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "image is not a regular file",
            ));
        }
        fs::read(path)
    }
    fn put(&self, _: &str, _: &[u8]) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "read-only"))
    }
    fn exists(&self, _: &str) -> io::Result<bool> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "read-only"))
    }
    fn delete(&self, _: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "read-only"))
    }
}

/// Load and verify a manifest and every managed file before any caller writes.
pub fn read_tree(root: &Path) -> Result<(Manifest, BTreeMap<String, Vec<u8>>)> {
    safe_path(root)?;
    let manifest_path = root.join("manifest.json");
    safe_path(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    if manifest.format_version != 1 {
        return Err(invalid("unsupported export format version"));
    }
    let mut files = BTreeMap::new();
    for (path, hash) in &manifest.files {
        if !managed(path) {
            return Err(invalid("unsafe managed path"));
        }
        let full = root.join(path);
        safe_path(&full)?;
        let bytes = fs::read(full)?;
        if checksum(&bytes) != *hash {
            return Err(invalid("export checksum mismatch"));
        }
        files.insert(path.clone(), bytes);
    }
    if !files.contains_key("ingredients.yaml") {
        return Err(invalid("missing ingredients registry"));
    }
    Ok((manifest, files))
}

/// Update only paths named by the previous manifest or the new snapshot.
/// Existing unowned files are never overwritten or removed.
pub fn write_tree(root: &Path, snapshot: Snapshot) -> Result<()> {
    safe_path(root)?;
    if root.exists() && !root.is_dir() {
        return Err(invalid("export destination is not a directory"));
    }
    let manifest_path = root.join("manifest.json");
    safe_path(&manifest_path)?;
    let old: BTreeSet<String> = if manifest_path.exists() {
        read_tree(root)?.0.files.into_keys().collect()
    } else {
        BTreeSet::new()
    };
    for path in snapshot.files.keys() {
        if !managed(path) {
            return Err(invalid("unsafe managed path"));
        }
        let full = root.join(path);
        safe_path(&full)?;
        if full.exists() && !old.contains(path) {
            return Err(invalid("export would overwrite an unrelated file"));
        }
    }
    let manifest = Manifest {
        format_version: 1,
        app_version: env!("CARGO_PKG_VERSION").into(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        recipes: snapshot.recipes,
        ingredients: snapshot.ingredients,
        files: snapshot
            .files
            .iter()
            .map(|(path, bytes)| (path.clone(), checksum(bytes)))
            .collect(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    // Stage all bytes before mutating the destination. The parent must already
    // exist; this keeps failed validation from creating arbitrary directories.
    let parent = root
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    safe_path(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    for (path, bytes) in &snapshot.files {
        let target = stage.path().join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::write(target, bytes)?;
    }
    fs::write(stage.path().join("manifest.json"), manifest_bytes)?;
    fs::create_dir_all(root)?;
    for path in snapshot.files.keys() {
        let dest = root.join(path);
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::rename(stage.path().join(path), dest)?;
    }
    for path in old {
        if !snapshot.files.contains_key(&path) {
            fs::remove_file(root.join(path))?;
        }
    }
    fs::rename(stage.path().join("manifest.json"), manifest_path)?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    ingredients: Vec<Ingredient>,
    aliases: Vec<Alias>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ingredient {
    id: String,
    name: String,
    normalized_name: String,
    redirect_to: Option<String>,
    deleted: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Alias {
    normalized_alias: String,
    alias: String,
    ingredient_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    ordinal: usize,
    wording: String,
    ingredient: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    id: String,
    title: String,
    description: String,
    servings: i64,
    tags: Vec<String>,
    source_url: Option<String>,
    notes: Option<String>,
    image: Option<String>,
    image_ref: Option<String>,
    updated_at: String,
    author: String,
    ingredient_links: Vec<Link>,
}

fn target_parent(path: &Path) -> Result<&Path> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    safe_path(path)?;
    if !parent.is_dir() || path.file_name().is_none() {
        return Err(invalid("target parent must exist"));
    }
    Ok(parent)
}

/// Restore into offline, fresh targets. All source validation and rebuilding is
/// completed in temporary directories before creating either target.
pub async fn import_tree(src: &Path, db: &Path, content: &Path) -> Result<()> {
    let (manifest, files) = read_tree(src)?;
    let db_parent = target_parent(db)?;
    let content_parent = target_parent(content)?;
    if db.exists() {
        return Err(invalid("import requires an absent database target"));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        safe_path(&sidecar)?;
        if sidecar.exists() {
            return Err(invalid("import target has an existing SQLite sidecar"));
        }
    }
    let content_existed = content.exists();
    if content_existed && (!content.is_dir() || fs::read_dir(content)?.next().is_some()) {
        return Err(invalid("import requires an empty content directory"));
    }
    let db_abs = db_parent.canonicalize()?.join(db.file_name().unwrap());
    let content_abs = content_parent
        .canonicalize()?
        .join(content.file_name().unwrap());
    let source_abs = src.canonicalize()?;
    if db_abs == content_abs
        || db_abs.starts_with(&content_abs)
        || content_abs.starts_with(&db_abs)
        || source_abs.starts_with(&content_abs)
        || content_abs.starts_with(&source_abs)
        || db_abs.starts_with(&source_abs)
    {
        return Err(invalid("import targets overlap each other or the export"));
    }
    let registry: Registry = serde_yaml::from_slice(&files["ingredients.yaml"])?;
    if registry.ingredients.len() != manifest.ingredients {
        return Err(invalid("ingredient count mismatch"));
    }
    let db_stage = tempfile::tempdir_in(db_parent)?;
    let content_stage = tempfile::tempdir_in(content_parent)?;
    let staged_db = db_stage.path().join("book.sqlite");
    let pool = database::connect(&staged_db).await?;
    database::migrate(&pool).await?;
    let mut tx = pool.begin().await?;
    for ingredient in &registry.ingredients {
        sqlx::query("INSERT INTO ingredients(id,name,normalized_name,deleted) VALUES(?,?,?,?)")
            .bind(&ingredient.id)
            .bind(&ingredient.name)
            .bind(&ingredient.normalized_name)
            .bind(ingredient.deleted)
            .execute(&mut *tx)
            .await?;
    }
    for ingredient in &registry.ingredients {
        if let Some(redirect) = &ingredient.redirect_to {
            sqlx::query("UPDATE ingredients SET redirect_to=? WHERE id=?")
                .bind(redirect)
                .bind(&ingredient.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    for alias in &registry.aliases {
        sqlx::query(
            "INSERT INTO ingredient_aliases(normalized_alias,alias,ingredient_id) VALUES(?,?,?)",
        )
        .bind(&alias.normalized_alias)
        .bind(&alias.alias)
        .bind(&alias.ingredient_id)
        .execute(&mut *tx)
        .await?;
    }
    let mut image_refs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut count = 0;
    for (path, bytes) in &files {
        if !path.starts_with("recipes/") {
            continue;
        }
        count += 1;
        let text = std::str::from_utf8(bytes)?;
        let (front, body) = text
            .strip_prefix("---\n")
            .ok_or_else(|| invalid("missing recipe front matter"))?
            .split_once("\n---\n")
            .ok_or_else(|| invalid("invalid recipe front matter"))?;
        let recipe: Recipe = serde_yaml::from_str(front)?;
        let deleted = path.starts_with("recipes/deleted/");
        match (&recipe.image, &recipe.image_ref) {
            (None, None) => {}
            (Some(image), Some(reference)) => {
                if !reference.starts_with("img_")
                    || !reference
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    return Err(invalid("invalid image ref"));
                }
                let prefix = if deleted { "../../" } else { "../" };
                let image_path = image
                    .strip_prefix(prefix)
                    .ok_or_else(|| invalid("invalid image path"))?;
                if !managed(image_path) || !image_path.starts_with("images/") {
                    return Err(invalid("invalid image path"));
                }
                let image_bytes = files
                    .get(image_path)
                    .ok_or_else(|| invalid("missing image"))?;
                if let Some(previous) = image_refs.insert(reference.clone(), image_bytes.clone()) {
                    if previous != *image_bytes {
                        return Err(invalid("image ref has conflicting bytes"));
                    }
                }
            }
            _ => return Err(invalid("image and image_ref must occur together")),
        }
        let revision = format!("rev_{}", uuid::Uuid::new_v4().simple());
        sqlx::query("INSERT INTO recipes(id,current_revision_id,deleted) VALUES(?,?,?)")
            .bind(&recipe.id)
            .bind(&revision)
            .bind(deleted as i64)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,source_url,notes,body,deleted,image_ref) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&revision).bind(&recipe.id).bind(&recipe.updated_at).bind(&recipe.author)
            .bind(&recipe.title).bind(&recipe.description).bind(recipe.servings)
            .bind(serde_json::to_string(&recipe.tags)?).bind(&recipe.source_url).bind(&recipe.notes)
            .bind(body).bind(deleted as i64).bind(&recipe.image_ref).execute(&mut *tx).await?;
        let words = ingredient_names(body);
        for link in &recipe.ingredient_links {
            if words.get(link.ordinal) != Some(&link.wording) {
                return Err(invalid("ingredient link does not match body"));
            }
            sqlx::query("INSERT INTO ingredient_resolution_provenance(revision_id,ordinal,normalized_wording,ingredient_id) VALUES(?,?,?,?)")
                .bind(&revision).bind(link.ordinal as i64).bind(normalize(&link.wording)).bind(&link.ingredient)
                .execute(&mut *tx).await?;
        }
    }
    if count != manifest.recipes {
        return Err(invalid("recipe count mismatch"));
    }
    tx.commit().await?;
    for (reference, bytes) in &image_refs {
        fs::write(content_stage.path().join(reference), bytes)?;
    }
    crate::rebuild_derived(&pool).await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    if integrity != "ok" {
        return Err(invalid("SQLite integrity check failed"));
    }
    let rebuilt =
        crate::book_snapshot::capture(&pool, &FileImageStorage::new(content_stage.path())).await?;
    if rebuilt.files != files {
        return Err(invalid("export does not round-trip faithfully"));
    }
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await?;
    pool.close().await;
    // Two paths cannot be published atomically. Publish the content directory
    // first and the database last; callers must keep the app stopped.
    if !content_existed {
        fs::create_dir(content)?;
    } else if fs::read_dir(content)?.next().is_some() {
        return Err(invalid("content directory is no longer empty"));
    }
    let mut installed = Vec::new();
    let result = (|| -> Result<()> {
        for (reference, bytes) in &image_refs {
            let path = content.join(reference);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            installed.push(path);
            file.write_all(bytes)?;
        }
        fs::hard_link(&staged_db, db)?;
        Ok(())
    })();
    if result.is_err() {
        for path in installed {
            let _ = fs::remove_file(path);
        }
        if !content_existed {
            let _ = fs::remove_dir(content);
        }
    }
    result
}
