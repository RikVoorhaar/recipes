use std::process::Command;
use tempfile::TempDir;

#[test]
fn token_lifecycle_is_persistent_and_never_lists_secrets() {
    let dir = TempDir::new().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_recipe-app"))
            .env("APP__DATABASE_PATH", dir.path().join("db.sqlite3"))
            .env("APP__CONTENT_DIR", dir.path().join("content"))
            .args(args)
            .output()
            .unwrap()
    };
    let created = run(&["token", "create", "--label", "agent"]);
    assert!(created.status.success());
    let secret = String::from_utf8(created.stdout).unwrap();
    assert!(secret.trim().starts_with("rcp_"));
    let listed = run(&["token", "list"]);
    let listing = String::from_utf8(listed.stdout).unwrap();
    assert!(listing.contains("agent") && listing.contains("active"));
    assert!(!listing.contains(secret.trim()));
    let rotated = run(&["token", "rotate", "--label", "agent"]);
    assert!(rotated.status.success());
    let replacement = String::from_utf8(rotated.stdout).unwrap();
    assert!(replacement.trim().starts_with("rcp_"));
    assert_ne!(secret, replacement);
    assert!(run(&["token", "revoke", "--label", "agent"])
        .status
        .success());
    assert!(String::from_utf8(run(&["token", "list"]).stdout)
        .unwrap()
        .contains("revoked"));
}
