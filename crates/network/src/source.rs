use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// An owned source outlives both the picker callback and the network task.
#[derive(Clone)]
pub enum SendSource {
    Batch(Vec<SendSource>),
    FolderCache {
        directory: Arc<CachedDirectory>,
        name: Arc<str>,
    },
    Path(PathBuf),
    /// A provider-backed regular file, with the logical document's name/length.
    OpenFile {
        file: Arc<Mutex<Option<File>>>,
        name: Arc<str>,
        size: u64,
        offset: u64,
        /// Persistently granted Android document, reopenable across process restarts.
        uri: Option<Arc<str>>,
    },
    /// Owned temporary cache, deleted when its last owner drops. Never use this
    /// variant for a user's original file.
    Cached {
        path: Arc<tempfile::TempPath>,
        name: Arc<str>,
    },
}

pub struct CachedDirectory(PathBuf);
impl Drop for CachedDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl SendSource {
    pub fn from_file(file: File, name: String, size: u64) -> Self {
        Self::from_file_region(file, name, 0, size)
    }

    /// A known-length region of an owned regular file. Bounds are checked again
    /// immediately before sending, including overflow and source truncation.
    pub fn from_file_region(file: File, name: String, offset: u64, size: u64) -> Self {
        Self::OpenFile {
            file: Arc::new(Mutex::new(Some(file))),
            name: name.into(),
            size,
            offset,
            uri: None,
        }
    }

    pub fn cached(path: PathBuf, name: String) -> std::io::Result<Self> {
        if path.is_dir() {
            return Ok(Self::FolderCache {
                directory: Arc::new(CachedDirectory(path)),
                name: name.into(),
            });
        }
        Ok(Self::Cached {
            path: Arc::new(tempfile::TempPath::try_from_path(path)?),
            name: name.into(),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Batch(_) => None,
            Self::FolderCache { directory, .. } => Some(&directory.0),
            Self::Path(path) => Some(path),
            Self::OpenFile { .. } => None,
            Self::Cached { path, .. } => Some(path.as_ref().as_ref()),
        }
    }

    /// Picker caches are private, finished exports: after handing them to Rust
    /// their owners only read or unlink them. Never use this for user paths.
    pub(crate) fn is_private_cache(&self) -> bool {
        matches!(self, Self::Cached { .. } | Self::FolderCache { .. })
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Batch(_) => Some("批量文件"),
            Self::FolderCache { name, .. } => Some(name),
            Self::OpenFile { name, .. } | Self::Cached { name, .. } => Some(name),
            _ => self.path()?.file_name()?.to_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_name_is_independent_and_cleanup_waits_for_last_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("random.part");
        std::fs::write(&path, b"data").unwrap();
        let source = SendSource::cached(path.clone(), "文件.bin".into()).unwrap();
        assert_eq!(source.name(), Some("文件.bin"));
        let second = source.clone();
        drop(source);
        assert!(path.exists());
        drop(second);
        assert!(!path.exists());
        std::fs::write(&path, b"original").unwrap();
        drop(SendSource::Path(path.clone()));
        assert!(path.exists());
    }
}
