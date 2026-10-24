use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};

pub(super) fn validate_component(part: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    super::super::filename::validate(part, true)
        .with_context(|| format!("不支持的文件名：{part}"))?;
    anyhow::ensure!(part.len() <= 240, "文件名超过 240 字节：{part}");
    anyhow::ensure!(
        !part.to_lowercase().starts_with(".portalo-"),
        "文件名使用了 Portalo 保留前缀：{part}"
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: Vec<String>,
    pub directory: bool,
    pub size: u64,
    pub blocks: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub entries: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Layout::is_legacy")]
    pub layout: Layout,
    /// Hashes arrive immediately before their blocks; path/size manifest stays immutable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub streaming: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    #[default]
    Legacy,
    File,
    Directory,
    Batch,
}
impl Layout {
    fn is_legacy(&self) -> bool {
        *self == Self::Legacy
    }
}

impl Manifest {
    pub fn digest(&self) -> anyhow::Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
    pub fn total(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(uuid::Uuid::parse_str(&self.id).is_ok(), "Invalid task ID");
        super::super::filename::validate(&self.name, true)?;
        anyhow::ensure!(self.name.len() <= 240, "Task name too long");
        anyhow::ensure!(self.entries.len() <= 100_000, "Invalid entry count");
        if self.layout == Layout::File {
            anyhow::ensure!(
                self.entries.len() == 1
                    && !self.entries[0].directory
                    && self.entries[0].path == [self.name.clone()],
                "Invalid single-file manifest"
            );
        }
        let mut paths = HashSet::new();
        let mut directories = HashSet::new();
        let mut exact_directories = HashSet::<Vec<String>>::new();
        let mut total = 0u64;
        let mut block_count = 0u64;
        for entry in &self.entries {
            block_count = block_count
                .checked_add(entry.size.div_ceil(super::BLOCK as u64))
                .ok_or_else(|| anyhow::anyhow!("Block count overflow"))?;
            anyhow::ensure!(
                block_count <= (super::wire::MAX_META / 2 / 67) as u64,
                "Block hash metadata limit exceeded"
            );
            anyhow::ensure!(
                !entry.path.is_empty() && entry.path.len() <= 64,
                "Invalid path depth"
            );
            let mut key = String::new();
            for (i, part) in entry.path.iter().enumerate() {
                validate_component(part).map_err(|error| {
                    anyhow::anyhow!("无法发送路径 {}：{error:#}", entry.path.join("/"))
                })?;
                if i > 0 {
                    anyhow::ensure!(directories.contains(&key), "Missing parent directory");
                    anyhow::ensure!(
                        exact_directories.contains(&entry.path[..i]),
                        "Parent directory casing mismatch"
                    );
                }
                key.push('/');
                key.push_str(&part.to_lowercase());
            }
            anyhow::ensure!(
                key.len() <= 4096 && paths.insert(key.clone()),
                "Duplicate or conflicting path"
            );
            if entry.directory {
                anyhow::ensure!(
                    entry.size == 0 && entry.blocks.is_empty(),
                    "Directory contains payload"
                );
                directories.insert(key);
                exact_directories.insert(entry.path.clone());
            } else {
                anyhow::ensure!(
                    if self.streaming {
                        entry.blocks.is_empty()
                    } else {
                        entry.blocks.len() as u64 == entry.size.div_ceil(super::BLOCK as u64)
                    },
                    "Invalid block count"
                );
                for hash in &entry.blocks {
                    anyhow::ensure!(
                        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                        "Invalid block digest"
                    );
                }
            }
            total = total
                .checked_add(entry.size)
                .ok_or_else(|| anyhow::anyhow!("Total size overflow"))?;
        }
        Ok(())
    }
    pub fn path(&self, root: &std::path::Path, index: usize) -> PathBuf {
        self.entries[index]
            .path
            .iter()
            .fold(root.to_owned(), |p, c| p.join(c))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedTask {
    pub manifest: Manifest,
    pub token: String,
    pub addresses: Vec<std::net::SocketAddr>,
    pub result: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub digest: String,
    pub token: String,
    pub offsets: Vec<u64>,
    pub result: Option<String>,
    pub target: PathBuf,
    pub document_tree: Option<String>,
    #[serde(default)]
    pub publishing: bool,
    /// Missing on old receipts: keep their existing staged backend.
    #[serde(default)]
    pub select_storage: bool,
    #[serde(default)]
    pub direct: Option<DirectTarget>,
    /// Streaming block hashes are committed atomically with durable offsets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hashes: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DirectTarget {
    pub root: String,
    // Re-enumerated by Java on resume, including provider URI changes after
    // rename. Share in memory and omit from per-checkpoint journal rewrites.
    #[serde(default, skip_serializing)]
    pub uris: std::sync::Arc<Vec<String>>,
    #[serde(default)]
    pub published: bool,
}
