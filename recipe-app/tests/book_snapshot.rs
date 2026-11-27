use recipe_app::{book_snapshot, database, storage::FileImageStorage};
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};

struct PausingImages {
    entered: Arc<Barrier>,
    resume: Arc<Barrier>,
}

impl recipe_app::storage::ImageStorage for PausingImages {
    fn get(&self, _: &str) -> std::io::Result<Vec<u8>> {
        self.entered.wait();
        self.resume.wait();
        Ok(b"image".to_vec())
    }
    fn put(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
        unreachable!()
    }
    fn exists(&self, _: &str) -> std::io::Result<bool> {
        unreachable!()
    }
    fn delete(&self, _: &str) -> std::io::Result<()> {
        unreachable!()
    }
}

#[tokio::test]
async fn snapshot_is_repeatable_and_excludes_private_state() {
    let dir = tempfile::tempdir().unwrap();
    let pool = database::connect(&dir.path().join("book.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    sqlx::query("INSERT INTO ingredients(id,name,normalized_name) VALUES('ing_salt','salt','salt'),('ing_pepper','pepper','pepper')")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO recipes(id,current_revision_id,deleted) VALUES('rcp_1234567890','rev_one',0),('rcp_abcdef1234','rev_two',1)")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body,deleted,image_ref) VALUES('rev_one','rcp_1234567890','2026-01-01T00:00:00Z','cook','Soup','',2,'[\"warm\"]','@salt{1%g}',0,'img_public'),('rev_two','rcp_abcdef1234','2026-01-02T00:00:00Z','cook','Old Soup','',1,'[]','water',1,NULL)")
        .execute(&pool).await.unwrap();
    // The saved identity differs from today's canonical spelling.
    sqlx::query(
        "INSERT INTO ingredient_resolution_provenance VALUES('rev_one',0,'salt','ing_pepper')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO api_tokens VALUES('secret-token-hash','private-label')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_image_refs VALUES('dimg_secret')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO recipe_drafts(id,owner_token_hash,revision,document,created_at,updated_at) VALUES('draft_secret','secret-token-hash',1,'private-draft-content','now','now')")
        .execute(&pool).await.unwrap();
    std::fs::write(dir.path().join("img_public"), b"public-image").unwrap();
    std::fs::write(dir.path().join("dimg_secret"), b"private-image").unwrap();
    let storage = FileImageStorage::new(dir.path());
    let first = book_snapshot::capture(&pool, &storage).await.unwrap();
    let second = book_snapshot::capture(&pool, &storage).await.unwrap();
    assert_eq!(first.files, second.files);
    assert_eq!((first.recipes, first.ingredients), (2, 2));
    assert_eq!(
        first.files["images/soup--rcp_12345678.jpg"],
        b"public-image"
    );
    let cook = String::from_utf8(first.files["recipes/soup--rcp_12345678.cook"].clone()).unwrap();
    assert!(cook.contains("ingredient: ing_pepper"));
    assert!(cook.ends_with("@salt{1%g}"));
    assert!(first
        .files
        .contains_key("recipes/deleted/old-soup--rcp_abcdef12.cook"));
    let all: Vec<u8> = first.files.values().flatten().copied().collect();
    for secret in [
        b"secret-token-hash".as_slice(),
        b"private-image",
        b"dimg_secret",
        b"private-draft-content",
    ] {
        assert!(!all.windows(secret.len()).any(|window| window == secret));
    }
    let paths: BTreeMap<_, _> = first.files.keys().map(|path| (path, ())).collect();
    assert_eq!(paths.len(), 4);
}

#[tokio::test]
async fn missing_saved_provenance_fails_instead_of_changing_identity() {
    let dir = tempfile::tempdir().unwrap();
    let pool = database::connect(&dir.path().join("book.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    sqlx::query("INSERT INTO recipes(id,current_revision_id) VALUES('rcp_1234567890','rev')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body) VALUES('rev','rcp_1234567890','now','cook','Soup','',1,'[]','@salt{1%g}')")
        .execute(&pool).await.unwrap();
    let result = book_snapshot::capture(&pool, &FileImageStorage::new(dir.path())).await;
    assert!(result.err().unwrap().to_string().contains("provenance"));
}

#[tokio::test]
async fn malicious_recipe_ids_are_not_used_as_paths() {
    let dir = tempfile::tempdir().unwrap();
    let pool = database::connect(&dir.path().join("book.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    sqlx::query("INSERT INTO recipes(id,current_revision_id) VALUES('rcp_../../escape','rev')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body) VALUES('rev','rcp_../../escape','now','cook','../escape','',1,'[]','water')")
        .execute(&pool).await.unwrap();
    assert!(
        book_snapshot::capture(&pool, &FileImageStorage::new(dir.path()))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_keeps_one_read_view_during_a_live_write() {
    let dir = tempfile::tempdir().unwrap();
    let pool = database::connect(&dir.path().join("book.sqlite"))
        .await
        .unwrap();
    database::migrate(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO ingredients(id,name,normalized_name) VALUES('ing_salt','salt','salt')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO recipes(id,current_revision_id) VALUES('rcp_1234567890','rev')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body,image_ref) VALUES('rev','rcp_1234567890','now','cook','Soup','',1,'[]','@salt{1%g}','img_public')")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO ingredient_resolution_provenance VALUES('rev',0,'salt','ing_salt')")
        .execute(&pool)
        .await
        .unwrap();
    let entered = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let image = PausingImages {
        entered: entered.clone(),
        resume: resume.clone(),
    };
    let snapshot_pool = pool.clone();
    let task = tokio::spawn(async move { book_snapshot::capture(&snapshot_pool, &image).await });
    tokio::task::spawn_blocking(move || entered.wait())
        .await
        .unwrap();
    sqlx::query("UPDATE ingredient_resolution_provenance SET normalized_wording='changed' WHERE revision_id='rev'")
        .execute(&pool).await.unwrap();
    tokio::task::spawn_blocking(move || resume.wait())
        .await
        .unwrap();
    let snapshot = task.await.unwrap().unwrap();
    assert!(snapshot
        .files
        .contains_key("recipes/soup--rcp_12345678.cook"));
}
