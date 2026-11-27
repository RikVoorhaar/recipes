use recipe_app::{book_archive, book_snapshot, database, storage::FileImageStorage};
use sha2::{Digest, Sha256};
use std::process::Command;

#[tokio::test]
async fn cli_export_reads_consistent_snapshot_while_writer_is_uncommitted() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("live.sqlite");
    let content = tmp.path().join("content");
    std::fs::create_dir(&content).unwrap();
    let pool = database::connect(&db).await.unwrap();
    database::migrate(&pool).await.unwrap();
    sqlx::query("INSERT INTO recipes(id,current_revision_id) VALUES('rcp_1234567890','rev')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body) VALUES('rev','rcp_1234567890','now','cook','Before','',1,'[]','water')")
        .execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE recipe_revisions SET title='After' WHERE id='rev'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let tree = tmp.path().join("tree");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_recipe-app"))
            .env("APP__DATABASE_PATH", &db)
            .env("APP__CONTENT_DIR", &content)
            .args(["export", "--dest"])
            .arg(&tree)
            .output()
            .unwrap()
    };
    let out = run();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (_, first) = book_archive::read_tree(&tree).unwrap();
    assert!(first.contains_key("recipes/before--rcp_12345678.cook"));
    tx.commit().await.unwrap();
    let out = run();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (_, second) = book_archive::read_tree(&tree).unwrap();
    assert!(second.contains_key("recipes/after--rcp_12345678.cook"));
    assert!(!tree.join("recipes/before--rcp_12345678.cook").exists());
}

#[tokio::test]
async fn cli_roundtrip_and_failed_import_leave_target_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let source_db = root.join("source.sqlite");
    let source_content = root.join("source-content");
    std::fs::create_dir(&source_content).unwrap();
    std::fs::write(source_content.join("img_soup"), b"image-bytes").unwrap();
    let pool = database::connect(&source_db).await.unwrap();
    database::migrate(&pool).await.unwrap();
    sqlx::query("INSERT INTO ingredients(id,name,normalized_name) VALUES('ing_salt','salt','salt'),('ing_pepper','pepper','pepper')")
        .execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO recipes(id,current_revision_id,deleted) VALUES('rcp_1234567890','rev',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body,deleted,image_ref) VALUES('rev','rcp_1234567890','2026-01-01','cook','Soup','',2,'[]','@salt{1%g}',1,'img_soup')")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO ingredient_resolution_provenance VALUES('rev',0,'salt','ing_pepper')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let tree = root.join("export");
    let bin = env!("CARGO_BIN_EXE_recipe-app");
    let run = |db: &std::path::Path,
               content: &std::path::Path,
               command: &str,
               flag: &str,
               tree: &std::path::Path| {
        Command::new(bin)
            .env("APP__DATABASE_PATH", db)
            .env("APP__CONTENT_DIR", content)
            .arg(command)
            .arg(flag)
            .arg(tree)
            .output()
            .unwrap()
    };
    let out = run(&source_db, &source_content, "export", "--dest", &tree);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let before = book_archive::read_tree(&tree).unwrap().1;
    std::fs::create_dir(tree.join(".git")).unwrap();
    std::fs::write(tree.join(".git/config"), b"git-metadata").unwrap();
    std::fs::write(tree.join("unrelated.txt"), b"keep").unwrap();
    let out = run(&source_db, &source_content, "export", "--dest", &tree);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(tree.join(".git/config")).unwrap(),
        b"git-metadata"
    );
    assert_eq!(std::fs::read(tree.join("unrelated.txt")).unwrap(), b"keep");
    let manifest_path = tree.join("manifest.json");
    let manifest_original = std::fs::read(&manifest_path).unwrap();
    let recipe_path = "recipes/deleted/soup--rcp_12345678.cook";
    let original = std::fs::read(tree.join(recipe_path)).unwrap();
    let changed = String::from_utf8(original.clone())
        .unwrap()
        .replace("ing_pepper", "ing_missing");
    std::fs::write(tree.join(recipe_path), changed.as_bytes()).unwrap();
    let mut modified: serde_json::Value = serde_json::from_slice(&manifest_original).unwrap();
    modified["files"][recipe_path] = format!("{:x}", Sha256::digest(changed.as_bytes())).into();
    std::fs::write(&manifest_path, serde_json::to_vec(&modified).unwrap()).unwrap();
    let failed_db = root.join("dangling.sqlite");
    let failed_content = root.join("dangling-content");
    let out = run(&failed_db, &failed_content, "import", "--src", &tree);
    assert!(!out.status.success());
    assert!(!failed_db.exists() && !failed_content.exists());
    std::fs::write(tree.join(recipe_path), original).unwrap();
    let mut unsupported: serde_json::Value = serde_json::from_slice(&manifest_original).unwrap();
    unsupported["format_version"] = 99.into();
    std::fs::write(&manifest_path, serde_json::to_vec(&unsupported).unwrap()).unwrap();
    let out = run(&failed_db, &failed_content, "import", "--src", &tree);
    assert!(!out.status.success());
    assert!(!failed_db.exists() && !failed_content.exists());
    std::fs::write(&manifest_path, manifest_original).unwrap();
    let occupied_db = root.join("occupied.sqlite");
    let occupied_content = root.join("occupied-content");
    std::fs::create_dir(&occupied_content).unwrap();
    std::fs::write(occupied_content.join("keep"), b"unrelated").unwrap();
    let out = run(&occupied_db, &occupied_content, "import", "--src", &tree);
    assert!(!out.status.success());
    assert!(!occupied_db.exists());
    assert_eq!(
        std::fs::read(occupied_content.join("keep")).unwrap(),
        b"unrelated"
    );
    let initialized_db = root.join("initialized.sqlite");
    let initialized = database::connect(&initialized_db).await.unwrap();
    database::migrate(&initialized).await.unwrap();
    let initial_schema: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&initialized)
        .await
        .unwrap();
    let out = run(
        &initialized_db,
        &root.join("initialized-content"),
        "import",
        "--src",
        &tree,
    );
    assert!(!out.status.success());
    assert_eq!(
        initial_schema,
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&initialized)
            .await
            .unwrap()
    );
    assert!(!root.join("initialized-content").exists());
    initialized.close().await;
    let target_db = root.join("target.sqlite");
    let target_content = root.join("target-content");
    let out = run(&target_db, &target_content, "import", "--src", &tree);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(target_content.join("img_soup")).unwrap(),
        b"image-bytes"
    );
    let target_pool = database::connect(&target_db).await.unwrap();
    let restored = book_snapshot::capture(&target_pool, &FileImageStorage::new(&target_content))
        .await
        .unwrap();
    assert_eq!(before, restored.files);
    let second = root.join("second");
    let out = run(&target_db, &target_content, "export", "--dest", &second);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(before, book_archive::read_tree(&second).unwrap().1);
    let out = run(&target_db, &target_content, "import", "--src", &tree);
    assert!(!out.status.success());
    std::fs::write(tree.join("ingredients.yaml"), b"tampered").unwrap();
    let bad_db = root.join("bad.sqlite");
    let bad_content = root.join("bad-content");
    let out = run(&bad_db, &bad_content, "import", "--src", &tree);
    assert!(!out.status.success());
    assert!(!bad_db.exists());
    assert!(!bad_content.exists());
}

#[test]
fn manifest_path_traversal_is_rejected_before_reading_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("manifest.json"), r#"{"format_version":1,"app_version":"x","exported_at":"now","recipes":0,"ingredients":0,"files":{"../outside":"x"}}"#).unwrap();
    assert!(book_archive::read_tree(tmp.path()).is_err());
}

#[test]
fn reexport_removes_only_previously_managed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    let snapshot = |recipe: &str| {
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            "ingredients.yaml".into(),
            b"ingredients: []\naliases: []\n".to_vec(),
        );
        files.insert(recipe.into(), b"recipe".to_vec());
        book_snapshot::Snapshot {
            files,
            recipes: 1,
            ingredients: 0,
        }
    };
    book_archive::write_tree(&root, snapshot("recipes/old--rcp_12345678.cook")).unwrap();
    std::fs::write(root.join("recipes/unrelated.txt"), b"keep").unwrap();
    book_archive::write_tree(&root, snapshot("recipes/new--rcp_12345678.cook")).unwrap();
    assert!(!root.join("recipes/old--rcp_12345678.cook").exists());
    assert_eq!(
        std::fs::read(root.join("recipes/unrelated.txt")).unwrap(),
        b"keep"
    );
    assert!(root.join("recipes/new--rcp_12345678.cook").exists());
}

#[cfg(unix)]
#[test]
fn referenced_image_symlinks_are_rejected() {
    use recipe_app::storage::ImageStorage;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("outside"), b"secret").unwrap();
    std::os::unix::fs::symlink(tmp.path().join("outside"), tmp.path().join("img_public")).unwrap();
    let images = book_archive::ExportImages(tmp.path().to_path_buf());
    assert!(images.get("img_public").is_err());
}

#[test]
fn export_does_not_overwrite_unmanaged_files_or_follow_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("ingredients.yaml"), b"unrelated").unwrap();
    let mut files = std::collections::BTreeMap::new();
    files.insert("ingredients.yaml".to_owned(), b"new".to_vec());
    let snapshot = book_snapshot::Snapshot {
        files,
        recipes: 0,
        ingredients: 0,
    };
    assert!(book_archive::write_tree(&root, snapshot).is_err());
    assert_eq!(
        std::fs::read(root.join("ingredients.yaml")).unwrap(),
        b"unrelated"
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(root.join("ingredients.yaml")).unwrap();
        std::os::unix::fs::symlink(tmp.path(), root.join("recipes")).unwrap();
        let mut files = std::collections::BTreeMap::new();
        files.insert("recipes/a--rcp_12345678.cook".to_owned(), b"new".to_vec());
        files.insert("ingredients.yaml".to_owned(), b"new".to_vec());
        assert!(book_archive::write_tree(
            &root,
            book_snapshot::Snapshot {
                files,
                recipes: 1,
                ingredients: 0
            }
        )
        .is_err());
        assert!(!tmp.path().join("a--rcp_12345678.cook").exists());
    }
}
