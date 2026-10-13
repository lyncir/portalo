use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preferences {
    pub accent: usize,
    pub download_directory: PathBuf,
    #[serde(default)]
    pub document_tree: Option<DocumentTree>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentTree {
    pub uri: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveDestination {
    Directory(PathBuf),
    DocumentTree(DocumentTree),
}

impl Preferences {
    pub fn receive_destination(&self) -> ReceiveDestination {
        match &self.document_tree {
            Some(tree) => ReceiveDestination::DocumentTree(tree.clone()),
            None => ReceiveDestination::Directory(self.download_directory.clone()),
        }
    }

    pub fn directory_label(&self) -> String {
        match &self.document_tree {
            Some(tree) => format!("系统文件夹：{}\n{}", tree.label, tree.uri),
            None => self.download_directory.display().to_string(),
        }
    }

    pub fn use_directory(&mut self, path: PathBuf) {
        self.download_directory = path;
        self.document_tree = None;
    }
}

pub fn data_directory() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        bevy::android::ANDROID_APP
            .get()
            .expect("Android initialized")
            .internal_data_path()
            .expect("Internal storage")
    }
    #[cfg(not(target_os = "android"))]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("portalo")
    }
}

pub fn default_download_directory() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        data_directory().join("downloads")
    }
    #[cfg(not(target_os = "android"))]
    {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("downloads")
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            accent: 0,
            download_directory: default_download_directory(),
            document_tree: None,
        }
    }
}

#[derive(Resource, Clone)]
pub struct SharedSettings(pub Arc<RwLock<Preferences>>);

impl Default for SharedSettings {
    fn default() -> Self {
        let mut preferences = std::fs::read(data_directory().join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Preferences>(&bytes).ok())
            .unwrap_or_default();
        preferences.accent = preferences.accent.min(2);
        if let Some(path) = std::env::var_os("PORTALO_DOWNLOAD_DIR") {
            preferences.use_directory(path.into());
        }
        Self(Arc::new(RwLock::new(preferences)))
    }
}

/// Called on a blocking worker; publish runtime settings only after persistence succeeds.
pub fn persist(preferences: &Preferences) -> std::io::Result<()> {
    persist_in(&data_directory(), preferences)
}

fn persist_in(directory: &std::path::Path, preferences: &Preferences) -> std::io::Result<()> {
    use std::io::Write;
    match preferences.receive_destination() {
        ReceiveDestination::Directory(path) => {
            std::fs::create_dir_all(&path)?;
            drop(tempfile::tempfile_in(path)?);
        }
        ReceiveDestination::DocumentTree(tree) => {
            #[cfg(target_os = "android")]
            crate::android_storage::validate_tree(&tree.uri)?;
            #[cfg(not(target_os = "android"))]
            {
                let _ = tree;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "系统文档目录仅支持安卓",
                ));
            }
        }
    }
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec_pretty(preferences)?)?;
    temporary
        .persist(directory.join("settings.json"))
        .map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistence_round_trip_and_failed_update_preserves_previous_settings() {
        let root = tempfile::tempdir().unwrap();
        let mut preferences = Preferences {
            accent: 2,
            download_directory: root.path().join("接收文件"),
            document_tree: None,
        };
        persist_in(root.path(), &preferences).unwrap();
        let saved = std::fs::read(root.path().join("settings.json")).unwrap();
        let loaded: Preferences = serde_json::from_slice(&saved).unwrap();
        assert_eq!(loaded.accent, 2);
        assert_eq!(loaded.download_directory, preferences.download_directory);
        let invalid = root.path().join("not-a-directory");
        std::fs::write(&invalid, b"file").unwrap();
        preferences.download_directory = invalid;
        assert!(persist_in(root.path(), &preferences).is_err());
        assert_eq!(
            std::fs::read(root.path().join("settings.json")).unwrap(),
            saved
        );
    }

    #[test]
    fn legacy_settings_and_explicit_destination_switches() {
        let mut preferences: Preferences =
            serde_json::from_str(r#"{"accent":1,"download_directory":"downloads"}"#).unwrap();
        assert_eq!(
            preferences.receive_destination(),
            ReceiveDestination::Directory("downloads".into())
        );
        let tree = DocumentTree {
            uri: "content://provider/tree/primary%3ADownload%2FPortalo".into(),
            label: "Portalo 文件夹".into(),
        };
        preferences.document_tree = Some(tree.clone());
        let loaded: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert_eq!(
            loaded.receive_destination(),
            ReceiveDestination::DocumentTree(tree)
        );
        preferences.use_directory("new-downloads".into());
        assert!(preferences.document_tree.is_none());
        assert_eq!(
            preferences.receive_destination(),
            ReceiveDestination::Directory("new-downloads".into())
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn document_uri_is_never_treated_as_a_filesystem_path() {
        let root = tempfile::tempdir().unwrap();
        let mut preferences = Preferences {
            accent: 0,
            download_directory: root.path().join("downloads"),
            document_tree: None,
        };
        persist_in(root.path(), &preferences).unwrap();
        let saved = std::fs::read(root.path().join("settings.json")).unwrap();
        preferences.document_tree = Some(DocumentTree {
            uri: "content://provider/tree/folder".into(),
            label: "文件夹".into(),
        });
        assert_eq!(
            persist_in(root.path(), &preferences).unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        assert_eq!(
            std::fs::read(root.path().join("settings.json")).unwrap(),
            saved
        );
    }
}
