use recipe_app::storage::ImageStorage;

/// Apply the shared behavioral contract to any storage backend.
pub fn assert_image_storage_contract(storage: &dyn ImageStorage) {
    let prefix = format!("contract-{}", uuid::Uuid::new_v4().simple());
    let key = format!("{prefix}-present");
    let missing = format!("{prefix}-missing");
    assert!(!storage.exists(&missing).unwrap());
    assert!(storage.get(&missing).is_err());

    storage.put(&key, b"opaque image bytes").unwrap();
    assert!(storage.exists(&key).unwrap());
    assert_eq!(storage.get(&key).unwrap(), b"opaque image bytes");

    storage.delete(&key).unwrap();
    assert!(!storage.exists(&key).unwrap());
    assert!(storage.get(&key).is_err());
    storage.delete(&key).unwrap();
}
