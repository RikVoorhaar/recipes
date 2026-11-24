#[allow(dead_code)]
mod storage_contract {
    include!("support/image_storage_contract.rs");
}

use recipe_app::storage::FileImageStorage;
use tempfile::TempDir;

#[test]
fn filesystem_backend_satisfies_shared_storage_contract() {
    let directory = TempDir::new().unwrap();
    storage_contract::assert_image_storage_contract(&FileImageStorage::new(directory.path()));
}
