//! Bounded, restartable capture: persist each snapshot block and its hash before
//! it can enter the network queue. Uncaptured ordinary files must remain intact.
use super::{BLOCK, Command, Entry, Layout, Manifest, SavedTask, storage};
use crate::SendSource;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Serialize, Deserialize)]
struct Source {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    #[serde(default)]
    offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    length: Option<u64>,
}
impl Source {
    fn inspect(path: &Path, file: &File) -> anyhow::Result<Self> {
        let meta = file.metadata()?;
        anyhow::ensure!(meta.is_file(), "源文件不是普通文件：{}", path.display());
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", meta.dev(), meta.ino())
        };
        #[cfg(windows)]
        let identity = {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
            };
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            // SAFETY: live file handle and correctly sized output structure.
            anyhow::ensure!(
                unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
                "无法读取源文件标识：{}",
                std::io::Error::last_os_error()
            );
            format!(
                "{}:{}:{}",
                info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
            )
        };
        Ok(Self {
            path: path.to_owned(),
            size: meta.len(),
            modified: meta.modified()?,
            identity,
            uri: None,
            offset: 0,
            length: None,
        })
    }
    fn check(&self, file: &File) -> anyhow::Result<()> {
        let current = Self::inspect(&self.path, file)?;
        anyhow::ensure!(
            current.size == self.size
                && current.modified == self.modified
                && current.identity == self.identity,
            "原文件已变化，尚未暂存的内容不能继续发送：{}",
            self.path.display()
        );
        Ok(())
    }
    fn open(&self) -> anyhow::Result<File> {
        if let Some(uri) = &self.uri {
            #[cfg(target_os = "android")]
            return Ok(portalo_core::android_storage::reopen_send_document(
                uri,
                self.offset,
                self.length
                    .ok_or_else(|| anyhow::anyhow!("Missing document length"))?,
            )?);
            #[cfg(not(target_os = "android"))]
            anyhow::bail!("Android document cannot be reopened on this platform: {uri}");
        }
        Ok(File::open(&self.path)?)
    }
}

#[derive(Serialize, Deserialize)]
struct Capture {
    source: Option<Source>,
    /// A reflink or private cache already owns the entire immutable payload.
    snapshot: bool,
    hashes: Vec<String>,
}

pub fn prepare_streaming_with_progress(
    sources: Vec<SendSource>,
    addresses: Vec<std::net::SocketAddr>,
    control: &tokio::sync::watch::Receiver<Command>,
    progress: &dyn Fn(u64, &str),
) -> anyhow::Result<SavedTask> {
    let sources: Vec<_> = sources
        .into_iter()
        .flat_map(|source| match source {
            SendSource::Batch(items) => items,
            source => vec![source],
        })
        .collect();
    // Unreopenable provider handles keep the stable, fully captured path.
    if sources.iter().any(|source| {
        source.path().is_none() && !matches!(source, SendSource::OpenFile { uri: Some(_), .. })
    }) {
        return storage::prepare_with_progress(sources, addresses, control, progress);
    }
    anyhow::ensure!(!sources.is_empty(), "没有选择文件");
    let _lock = storage::preparation_lock(true)?;
    let id = uuid::Uuid::new_v4().to_string();
    let root = storage::outgoing(&id);
    fs::create_dir_all(root.join("data"))?;
    let result = (|| {
        let directory = sources.len() == 1 && sources[0].path().is_some_and(Path::is_dir);
        let mut manifest = Manifest {
            name: if sources.len() == 1 {
                sources[0].name().unwrap_or("文件").to_owned()
            } else {
                format!("Portalo-{}", &id[..8])
            },
            id,
            entries: vec![],
            layout: if directory {
                Layout::Directory
            } else if sources.len() == 1 {
                Layout::File
            } else {
                Layout::Batch
            },
            streaming: true,
        };
        for source in sources {
            if let SendSource::OpenFile {
                file,
                name,
                size,
                offset,
                uri: Some(uri),
            } = &source
            {
                anyhow::ensure!(*control.borrow() == Command::Run, "Preparation interrupted");
                super::model::validate_component(name)?;
                anyhow::ensure!(uri.starts_with("content://"), "Invalid document URI");
                let file = file
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Source lock"))?
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("Source already consumed"))?;
                let mut state = Source::inspect(Path::new(name.as_ref()), &file)?;
                anyhow::ensure!(
                    *offset <= state.size && *size <= state.size - *offset,
                    "Document region outside file"
                );
                state.uri = Some(uri.to_string());
                state.offset = *offset;
                state.length = Some(*size);
                // Verify the saved URI really reopens the same object before
                // registering a restartable task. No payload is read here.
                state.check(&state.open()?)?;
                let target = root.join("data").join(manifest.entries.len().to_string());
                File::create(&target)?.sync_all()?;
                storage::save(
                    &target.with_extension("capture.json"),
                    &Capture {
                        source: Some(state),
                        snapshot: false,
                        hashes: vec![],
                    },
                )?;
                manifest.entries.push(Entry {
                    path: vec![name.to_string()],
                    directory: false,
                    size: *size,
                    blocks: vec![],
                });
                progress(0, name);
                continue;
            }
            let path = source.path().unwrap();
            check_type(path)?;
            if directory {
                for entry in fs::read_dir(path)? {
                    let entry = entry?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("Non UTF-8 filename"))?;
                    scan(
                        &entry.path(),
                        vec![name],
                        source.is_private_cache(),
                        &root,
                        &mut manifest,
                        control,
                        progress,
                    )?;
                }
            } else {
                scan(
                    path,
                    vec![source.name().unwrap_or("文件").to_owned()],
                    source.is_private_cache(),
                    &root,
                    &mut manifest,
                    control,
                    progress,
                )?;
            }
        }
        manifest.validate()?;
        anyhow::ensure!(
            serde_json::to_vec(&manifest)?.len() < super::wire::MAX_META / 2,
            "Manifest too large"
        );
        let task = SavedTask {
            manifest,
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            addresses,
            result: None,
        };
        #[cfg(unix)]
        File::open(root.join("data"))?.sync_all()?;
        storage::save(&root.join("task.json"), &task)?;
        Ok(task)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(root);
    }
    result
}

fn check_type(path: &Path) -> anyhow::Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        !meta.file_type().is_symlink() && (meta.is_file() || meta.is_dir()),
        "不支持的路径：{}",
        path.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            meta.file_attributes() & 0x400 == 0,
            "不支持重解析点：{}",
            path.display()
        );
    }
    Ok(meta)
}

fn scan(
    path: &Path,
    relative: Vec<String>,
    private: bool,
    root: &Path,
    manifest: &mut Manifest,
    control: &tokio::sync::watch::Receiver<Command>,
    progress: &dyn Fn(u64, &str),
) -> anyhow::Result<()> {
    anyhow::ensure!(*control.borrow() == Command::Run, "Preparation interrupted");
    anyhow::ensure!(
        relative.len() <= 64 && manifest.entries.len() < 100_000,
        "Directory limits exceeded"
    );
    for part in &relative {
        super::model::validate_component(part)
            .map_err(|error| anyhow::anyhow!("无法发送路径 {}：{error:#}", path.display()))?;
    }
    let meta = check_type(path)?;
    let index = manifest.entries.len();
    manifest.entries.push(Entry {
        path: relative.clone(),
        directory: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        blocks: vec![],
    });
    progress(0, &relative.join("/"));
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let mut next = relative.clone();
            next.push(
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Non UTF-8 filename"))?,
            );
            scan(
                &entry.path(),
                next,
                private,
                root,
                manifest,
                control,
                progress,
            )?;
        }
        return Ok(());
    }
    let target = root.join("data").join(index.to_string());
    let mut input = File::open(path)?;
    let source = Source::inspect(&fs::canonicalize(path)?, &input)?;
    anyhow::ensure!(
        source.size == meta.len(),
        "准备期间源文件已变化：{}",
        path.display()
    );
    let linked = private && fs::hard_link(path, &target).is_ok();
    let mut output = File::options()
        .read(true)
        .write(true)
        .create_new(!linked)
        .open(&target)?;
    let mut snapshot =
        linked || storage::clone_snapshot(&mut input, &output, source.size, control)?;
    // A cache owner will delete its original name when preparation returns.
    // If linking/cloning is unavailable it must first be copied into task storage.
    if private && !snapshot {
        let mut buffer = vec![0; BLOCK];
        let mut remaining = source.size;
        while remaining > 0 {
            anyhow::ensure!(*control.borrow() == Command::Run, "Preparation interrupted");
            let count = remaining.min(BLOCK as u64) as usize;
            input.read_exact(&mut buffer[..count])?;
            output.write_all(&buffer[..count])?;
            remaining -= count as u64;
            progress(count as u64, &relative.join("/"));
        }
        snapshot = true;
    }
    source.check(&input)?;
    output.sync_all()?;
    storage::save(
        &target.with_extension("capture.json"),
        &Capture {
            source: if snapshot { None } else { Some(source) },
            snapshot,
            hashes: vec![],
        },
    )?;
    Ok(())
}

/// Persist the first block immediately, then capture bounded disk batches.
/// Each batch opens the source once and syncs payload + journal once, rather
/// than forcing three storage barriers and a provider reopen per 4 MiB.
pub(super) fn block(path: &Path, size: u64, offset: u64) -> anyhow::Result<(Vec<u8>, String)> {
    anyhow::ensure!(
        offset < size && offset.is_multiple_of(BLOCK as u64),
        "Invalid capture offset"
    );
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("capture.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let journal = path.with_extension("capture.json");
    let mut capture: Capture = serde_json::from_slice(&fs::read(&journal)?)?;
    let index = (offset / BLOCK as u64) as usize;
    anyhow::ensure!(index <= capture.hashes.len(), "Missing snapshot checkpoint");
    let count = (size - offset).min(BLOCK as u64) as usize;
    let mut buffer = vec![0; count];
    let mut snapshot = File::options().read(true).write(true).open(path)?;
    if index < capture.hashes.len() || capture.snapshot {
        snapshot.seek(SeekFrom::Start(offset))?;
        snapshot.read_exact(&mut buffer)?;
    } else {
        let source = capture
            .source
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing capture source"))?;
        let mut input = source.open().map_err(|error| {
            anyhow::anyhow!(
                "尚未暂存的原文件不可读取，请保留原文件后继续：{}：{error}",
                source.path.display()
            )
        })?;
        source.check(&input)?;
        if let Some(length) = source.length {
            anyhow::ensure!(length == size, "Document size changed");
        }
        input.seek(SeekFrom::Start(
            source
                .offset
                .checked_add(offset)
                .ok_or_else(|| anyhow::anyhow!("Document offset overflow"))?,
        ))?;
        input.read_exact(&mut buffer)?;
        snapshot.seek(SeekFrom::Start(offset))?;
        snapshot.write_all(&buffer)?;
        let first_hash = blake3::hash(&buffer).to_hex().to_string();
        capture.hashes.push(first_hash.clone());
        // First payload remains low latency. Later batches reuse a single
        // scratch block (not a 32 MiB allocation) and the same provider FD.
        let end = if offset == 0 {
            offset + count as u64
        } else {
            size.min(offset.saturating_add(32 * 1024 * 1024))
        };
        let mut position = offset + count as u64;
        if position < end {
            let mut scratch = vec![0; BLOCK];
            while position < end {
                let count = (end - position).min(BLOCK as u64) as usize;
                input.read_exact(&mut scratch[..count])?;
                snapshot.write_all(&scratch[..count])?;
                capture
                    .hashes
                    .push(blake3::hash(&scratch[..count]).to_hex().to_string());
                position += count as u64;
            }
        }
        source.check(&input)?;
        snapshot.sync_all()?;
        if position == size {
            capture.source = None;
            capture.snapshot = true;
        }
        storage::save(&journal, &capture)?;
        return Ok((buffer, first_hash));
    }
    let hash = blake3::hash(&buffer).to_hex().to_string();
    if index < capture.hashes.len() {
        anyhow::ensure!(capture.hashes[index] == hash, "Source snapshot corrupted");
    } else {
        capture.hashes.push(hash.clone());
        if offset + count as u64 == size {
            capture.source = None;
            capture.snapshot = true;
        }
        storage::save(&journal, &capture)?;
    }
    Ok((buffer, hash))
}

/// Prepare the next durable disk batch. The sender consumes already committed
/// ranges independently; it must not take the writer's lock while the next
/// batch is being read or synchronized.
pub(super) fn capture_ahead(path: &Path, size: u64, offset: u64) -> anyhow::Result<u64> {
    drop(block(path, size, offset)?);
    let capture: Capture = serde_json::from_slice(&fs::read(path.with_extension("capture.json"))?)?;
    Ok((capture.hashes.len() as u64 * BLOCK as u64).min(size))
}

pub(super) fn read_captured(
    path: &Path,
    size: u64,
    offset: u64,
) -> anyhow::Result<(Vec<u8>, String)> {
    anyhow::ensure!(
        offset < size && offset.is_multiple_of(BLOCK as u64),
        "Invalid capture offset"
    );
    let capture: Capture = serde_json::from_slice(&fs::read(path.with_extension("capture.json"))?)?;
    let hash = capture
        .hashes
        .get((offset / BLOCK as u64) as usize)
        .ok_or_else(|| anyhow::anyhow!("Snapshot block not durable yet"))?
        .clone();
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; (size - offset).min(BLOCK as u64) as usize];
    file.read_exact(&mut bytes)?;
    anyhow::ensure!(
        blake3::hash(&bytes).to_hex().as_str() == hash,
        "Source snapshot corrupted"
    );
    Ok((bytes, hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_batch_is_durable_bounded_and_readable_without_original() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let size = 12 * BLOCK as u64;
        File::create(&original).unwrap().set_len(size).unwrap();
        let source = Source::inspect(&original, &File::open(&original).unwrap()).unwrap();
        let target = dir.path().join("snapshot");
        File::create(&target).unwrap();
        storage::save(
            &target.with_extension("capture.json"),
            &Capture {
                source: Some(source),
                snapshot: false,
                hashes: vec![],
            },
        )
        .unwrap();
        block(&target, size, 0).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().len(), BLOCK as u64);
        block(&target, size, BLOCK as u64).unwrap();
        let state: Capture =
            serde_json::from_slice(&fs::read(target.with_extension("capture.json")).unwrap())
                .unwrap();
        assert_eq!(state.hashes.len(), 9);
        assert_eq!(fs::metadata(&target).unwrap().len(), 9 * BLOCK as u64);
        fs::remove_file(original).unwrap();
        assert_eq!(
            block(&target, size, 8 * BLOCK as u64).unwrap().0,
            vec![0; BLOCK]
        );
        assert!(block(&target, size, 9 * BLOCK as u64).is_err());
    }

    #[test]
    fn committed_block_reads_do_not_wait_for_next_batch_writer_lock() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        fs::write(&original, vec![23; 2 * BLOCK]).unwrap();
        let source = Source::inspect(&original, &File::open(&original).unwrap()).unwrap();
        let target = dir.path().join("snapshot");
        File::create(&target).unwrap();
        storage::save(
            &target.with_extension("capture.json"),
            &Capture {
                source: Some(source),
                snapshot: false,
                hashes: vec![],
            },
        )
        .unwrap();
        capture_ahead(&target, 2 * BLOCK as u64, 0).unwrap();
        let lock = File::options()
            .read(true)
            .write(true)
            .open(target.with_extension("capture.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(read_captured(&target, 2 * BLOCK as u64, 0))
                .unwrap()
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(3));
        drop(lock);
        worker.join().unwrap();
        assert_eq!(result.unwrap().unwrap().0, vec![23; BLOCK]);
    }

    #[test]
    fn document_region_capture_preserves_bounds_and_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider");
        let payload = vec![19; BLOCK + 23];
        let mut contents = b"header!".to_vec();
        contents.extend_from_slice(&payload);
        contents.extend_from_slice(b"tail");
        fs::write(&path, contents).unwrap();
        let mut source = Source::inspect(&path, &File::open(&path).unwrap()).unwrap();
        source.offset = 7;
        source.length = Some(payload.len() as u64);
        let target = dir.path().join("snapshot");
        File::create(&target).unwrap();
        storage::save(
            &target.with_extension("capture.json"),
            &Capture {
                source: Some(source),
                snapshot: false,
                hashes: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            block(&target, payload.len() as u64, 0).unwrap().0,
            payload[..BLOCK]
        );
        // Each invocation reloads the durable descriptor and resumes its exact region.
        assert_eq!(
            block(&target, payload.len() as u64, BLOCK as u64)
                .unwrap()
                .0,
            payload[BLOCK..]
        );
        assert_eq!(fs::read(&target).unwrap(), payload);
        fs::remove_file(path).unwrap();
        assert_eq!(
            block(&target, payload.len() as u64, 0).unwrap().0,
            payload[..BLOCK]
        );
    }

    #[test]
    fn large_file_starts_without_hashing_payload_and_captured_blocks_survive_source_removal() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("large.bin");
        File::create(&path)
            .unwrap()
            .set_len(8 * 1024 * 1024 * 1024)
            .unwrap();
        let (_tx, control) = tokio::sync::watch::channel(Command::Run);
        let task = prepare_streaming_with_progress(
            vec![SendSource::Path(path.clone())],
            vec![],
            &control,
            &|bytes, _| assert_eq!(bytes, 0),
        )
        .unwrap();
        assert!(task.manifest.streaming);
        assert!(task.manifest.entries[0].blocks.is_empty());
        let snapshot = storage::outgoing(&task.manifest.id).join("data/0");
        let capture: Capture =
            serde_json::from_slice(&fs::read(snapshot.with_extension("capture.json")).unwrap())
                .unwrap();
        assert!(capture.hashes.is_empty());
        let size = task.manifest.entries[0].size;
        let (bytes, hash) = block(&snapshot, size, 0).unwrap();
        assert_eq!(bytes, vec![0; BLOCK]);
        fs::remove_file(&path).unwrap();
        assert_eq!(block(&snapshot, size, 0).unwrap(), (bytes, hash));
        if !capture.snapshot {
            assert!(block(&snapshot, size, BLOCK as u64).is_err());
        }
        storage::discard(&task.manifest.id).unwrap();
    }

    #[test]
    fn uncaptured_source_replacement_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::write(&source, vec![7; BLOCK + 17]).unwrap();
        let input = File::open(&source).unwrap();
        let state = Capture {
            source: Some(Source::inspect(&source, &input).unwrap()),
            snapshot: false,
            hashes: vec![],
        };
        drop(input);
        let snapshot = directory.path().join("snapshot");
        File::create(&snapshot).unwrap();
        storage::save(&snapshot.with_extension("capture.json"), &state).unwrap();
        block(&snapshot, (BLOCK + 17) as u64, 0).unwrap();
        fs::rename(&source, directory.path().join("original")).unwrap();
        fs::write(&source, vec![8; BLOCK + 17]).unwrap();
        assert!(
            block(&snapshot, (BLOCK + 17) as u64, BLOCK as u64)
                .unwrap_err()
                .to_string()
                .contains("原文件已变化")
        );
        assert_eq!(
            block(&snapshot, (BLOCK + 17) as u64, 0).unwrap().0,
            vec![7; BLOCK]
        );
    }
}
