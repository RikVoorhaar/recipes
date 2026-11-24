use std::{fs, io, path::PathBuf};

/// Backend-independent access to immutable image objects.
pub trait ImageStorage: Send + Sync {
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
    fn get(&self, key: &str) -> io::Result<Vec<u8>>;
    fn exists(&self, key: &str) -> io::Result<bool>;
    /// Only maintenance tooling may call this; recipe writes never delete assets.
    fn delete(&self, key: &str) -> io::Result<()>;
}

#[derive(Clone)]
pub struct FileImageStorage {
    root: PathBuf,
}

impl FileImageStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, key: &str) -> io::Result<PathBuf> {
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid image key",
            ));
        }
        Ok(self.root.join(key))
    }
}

impl ImageStorage for FileImageStorage {
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let path = self.path(key)?;
        let temporary = path.with_extension("uploading");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }

    fn get(&self, key: &str) -> io::Result<Vec<u8>> {
        fs::read(self.path(key)?)
    }

    fn exists(&self, key: &str) -> io::Result<bool> {
        Ok(self.path(key)?.is_file())
    }

    fn delete(&self, key: &str) -> io::Result<()> {
        match fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
