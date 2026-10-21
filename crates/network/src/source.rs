use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// An owned source outlives both the picker callback and the network task.
#[derive(Clone)]
pub enum SendSource {
    Path(PathBuf),
    /// A provider-backed regular file, with the logical document's name/length.
    OpenFile {
        file: Arc<Mutex<Option<File>>>,
        name: Arc<str>,
        size: u64,
    },
    /// Owned temporary cache, deleted when its last owner drops. Never use this
    /// variant for a user's original file.
    Cached {
        path: Arc<tempfile::TempPath>,
        name: Arc<str>,
    },
}

impl SendSource {
    pub fn from_file(file: File, name: String, size: u64) -> Self {
        Self::OpenFile {
            file: Arc::new(Mutex::new(Some(file))),
            name: name.into(),
            size,
        }
    }

    pub fn cached(path: PathBuf, name: String) -> std::io::Result<Self> {
        Ok(Self::Cached {
            path: Arc::new(tempfile::TempPath::try_from_path(path)?),
            name: name.into(),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path),
            Self::OpenFile { .. } => None,
            Self::Cached { path, .. } => Some(path.as_ref().as_ref()),
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
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
