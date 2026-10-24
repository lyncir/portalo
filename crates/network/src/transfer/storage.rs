use super::{BLOCK, Entry, Manifest, SavedTask, model::Receipt};
use fs2::FileExt;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(super) fn base() -> PathBuf {
    #[cfg(test)]
    {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| tempfile::tempdir().unwrap())
            .path()
            .to_owned()
    }
    #[cfg(not(test))]
    {
        portalo_core::settings::data_directory().join("transfers-v2")
    }
}
pub(super) fn outgoing(id: &str) -> PathBuf {
    base().join("outgoing").join(id)
}
pub(super) fn preparation_lock(wait: bool) -> anyhow::Result<File> {
    fs::create_dir_all(base())?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(base().join("preparation.lock"))?;
    if wait {
        FileExt::lock_exclusive(&lock)?;
    } else {
        lock.try_lock_exclusive()?;
    }
    Ok(lock)
}
pub(super) fn save<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut tmp, value)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn tasks() -> anyhow::Result<Vec<SavedTask>> {
    // Only collect interrupted snapshots when no process is preparing one.
    let cleanup = preparation_lock(false).ok();
    let root = base().join("outgoing");
    fs::create_dir_all(&root)?;
    let mut tasks = Vec::new();
    for directory in fs::read_dir(root)? {
        let directory = directory?.path();
        let path = directory.join("task.json");
        if path.is_file() {
            let task: SavedTask = serde_json::from_slice(&fs::read(path)?)?;
            if task.result.is_none() {
                tasks.push(task);
            } else if cleanup.is_some() && directory.join("data").exists() {
                fs::remove_dir_all(directory.join("data"))?;
            }
        } else if cleanup.is_some()
            && directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok())
            && fs::symlink_metadata(&directory)?.is_dir()
        {
            fs::remove_dir_all(directory)?;
        }
    }
    tasks.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(tasks)
}
pub fn discard(id: &str) -> anyhow::Result<()> {
    uuid::Uuid::parse_str(id)?;
    let path = outgoing(id);
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

/// Immutable on-disk snapshots make source mutations and application restart
/// deterministic. Memory is bounded to one block while scanning/copying.
pub fn prepare(
    sources: Vec<super::super::SendSource>,
    addresses: Vec<std::net::SocketAddr>,
    control: &tokio::sync::watch::Receiver<super::Command>,
) -> anyhow::Result<SavedTask> {
    prepare_with_progress(sources, addresses, control, &|_, _| {})
}

pub fn prepare_with_progress(
    sources: Vec<super::super::SendSource>,
    addresses: Vec<std::net::SocketAddr>,
    control: &tokio::sync::watch::Receiver<super::Command>,
    progress: &dyn Fn(u64, &str),
) -> anyhow::Result<SavedTask> {
    let started = Instant::now();
    let _preparation = preparation_lock(true)?;
    let sources: Vec<_> = sources
        .into_iter()
        .flat_map(|source| match source {
            super::super::SendSource::Batch(sources) => sources,
            source => vec![source],
        })
        .collect();
    let id = uuid::Uuid::new_v4().to_string();
    let root = outgoing(&id);
    fs::create_dir_all(root.join("data"))?;
    let result = (|| {
        let name = if sources.len() == 1 {
            sources[0].name().unwrap_or("传输").to_owned()
        } else {
            format!("Portalo-{}", &id[..8])
        };
        let mut manifest = Manifest {
            streaming: false,
            id,
            name,
            entries: Vec::new(),
            layout: if sources.len() == 1 {
                if sources[0].path().is_some_and(|p| p.is_dir()) {
                    super::Layout::Directory
                } else {
                    super::Layout::File
                }
            } else {
                super::Layout::Batch
            },
        };
        let single_directory = sources.len() == 1 && sources[0].path().is_some_and(|p| p.is_dir());
        for source in sources {
            anyhow::ensure!(
                *control.borrow() == super::Command::Run,
                "Preparation interrupted"
            );
            let name = source.name().unwrap_or("文件").to_owned();
            if let Some(path) = source.path() {
                if single_directory {
                    anyhow::ensure!(
                        !fs::symlink_metadata(path)?.file_type().is_symlink(),
                        "Symbolic links are not supported"
                    );
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt;
                        anyhow::ensure!(
                            fs::symlink_metadata(path)?.file_attributes() & 0x400 == 0,
                            "Reparse points are not supported"
                        );
                    }
                    for child in fs::read_dir(path)? {
                        let child = child?;
                        let name = child
                            .file_name()
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("Non UTF-8 filename"))?;
                        scan(
                            &child.path(),
                            source.is_private_cache(),
                            vec![name],
                            &root.join("data"),
                            &mut manifest,
                            control,
                            progress,
                        )?;
                    }
                } else {
                    scan(
                        path,
                        source.is_private_cache(),
                        vec![name],
                        &root.join("data"),
                        &mut manifest,
                        control,
                        progress,
                    )?;
                }
            } else if let super::super::SendSource::OpenFile {
                file, size, offset, ..
            } = source
            {
                let mut file = file
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Source lock"))?
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("Source already consumed"))?;
                file.seek(SeekFrom::Start(offset))?;
                snapshot(
                    &mut file,
                    size,
                    vec![name],
                    &root.join("data"),
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
        save(&root.join("task.json"), &task)?;
        Ok(task)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(root);
    }
    bevy::log::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        success = result.is_ok(),
        "Transfer preparation finished"
    );
    result
}
#[allow(clippy::too_many_arguments)]
fn scan(
    path: &Path,
    private_cache: bool,
    relative: Vec<String>,
    target: &Path,
    manifest: &mut Manifest,
    control: &tokio::sync::watch::Receiver<super::Command>,
    progress: &dyn Fn(u64, &str),
) -> anyhow::Result<()> {
    anyhow::ensure!(
        *control.borrow() == super::Command::Run,
        "Preparation interrupted"
    );
    anyhow::ensure!(
        relative.len() <= 64 && manifest.entries.len() < 100_000,
        "Directory limits exceeded"
    );
    for part in &relative {
        super::model::validate_component(part)
            .map_err(|error| anyhow::anyhow!("无法发送路径 {}：{error:#}", path.display()))?;
    }
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        !meta.file_type().is_symlink(),
        "Symbolic links are not supported: {}",
        path.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            meta.file_attributes() & 0x400 == 0,
            "Reparse points are not supported"
        );
    }
    if meta.is_dir() {
        manifest.entries.push(Entry {
            path: relative.clone(),
            directory: true,
            size: 0,
            blocks: vec![],
        });
        for child in fs::read_dir(path)? {
            let child = child?;
            let mut next = relative.clone();
            next.push(
                child
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Non UTF-8 filename"))?,
            );
            scan(
                &child.path(),
                private_cache,
                next,
                target,
                manifest,
                control,
                progress,
            )?;
        }
    } else {
        anyhow::ensure!(meta.is_file(), "Only regular files are supported");
        let mut file = File::open(path)?;
        if private_cache && adopt_cache(path, &relative, target, manifest, control, progress)? {
            return Ok(());
        }
        snapshot(
            &mut file,
            meta.len(),
            relative,
            target,
            manifest,
            control,
            progress,
        )?;
        let after = file.metadata()?;
        anyhow::ensure!(
            after.len() == meta.len() && after.modified()? == meta.modified()?,
            "Source changed during preparation"
        );
    }
    Ok(())
}

/// Link only application-owned immutable exports. Dropping the picker owner
/// removes its name, not the snapshot's link; no second payload copy is needed.
fn adopt_cache(
    source: &Path,
    relative: &[String],
    target: &Path,
    manifest: &mut Manifest,
    control: &tokio::sync::watch::Receiver<super::Command>,
    progress: &dyn Fn(u64, &str),
) -> anyhow::Result<bool> {
    let target = target.join(manifest.entries.len().to_string());
    if let Err(error) = fs::hard_link(source, &target) {
        if target.try_exists()? {
            return Err(error.into());
        }
        // Cross-device caches and filesystems without hard links retain the
        // normal reflink/copy path, where actual I/O errors are reported.
        bevy::log::debug!(%error,"Private cache adoption unavailable");
        return Ok(false);
    }
    let started = Instant::now();
    let mut file = File::options().read(true).write(true).open(&target)?;
    let size = file.metadata()?.len();
    let mut blocks = Vec::new();
    let mut buffer = vec![0; BLOCK];
    let mut remaining = size;
    while remaining > 0 {
        anyhow::ensure!(
            *control.borrow() == super::Command::Run,
            "Preparation interrupted"
        );
        let count = remaining.min(BLOCK as u64) as usize;
        file.read_exact(&mut buffer[..count])?;
        blocks.push(blake3::hash(&buffer[..count]).to_hex().to_string());
        remaining -= count as u64;
        progress(count as u64, &relative.join("/"));
    }
    file.sync_all()?;
    manifest.entries.push(Entry {
        path: relative.to_vec(),
        directory: false,
        size,
        blocks,
    });
    bevy::log::info!(
        bytes = size,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Private export adopted without payload copy"
    );
    Ok(true)
}
fn snapshot(
    input: &mut File,
    size: u64,
    path: Vec<String>,
    root: &Path,
    manifest: &mut Manifest,
    control: &tokio::sync::watch::Receiver<super::Command>,
    progress: &dyn Fn(u64, &str),
) -> anyhow::Result<()> {
    for part in &path {
        super::model::validate_component(part)
            .map_err(|error| anyhow::anyhow!("无法发送路径 {}：{error:#}", path.join("/")))?;
    }
    let started = Instant::now();
    let metrics = super::metrics::Metrics::default();
    let mut output = File::options()
        .create_new(true)
        .read(true)
        .write(true)
        .open(root.join(manifest.entries.len().to_string()))?;
    let cloned = clone_snapshot(input, &output, size, control)?;
    if cloned {
        output.seek(SeekFrom::Start(0))?;
    }
    let mut remaining = size;
    let mut blocks = Vec::new();
    let mut buffer = vec![0; BLOCK];
    while remaining > 0 {
        anyhow::ensure!(
            *control.borrow() == super::Command::Run,
            "Preparation interrupted"
        );
        let count = remaining.min(BLOCK as u64) as usize;
        {
            let _time = super::metrics::measure(&metrics.write);
            if cloned {
                output.read_exact(&mut buffer[..count])?;
            } else {
                input.read_exact(&mut buffer[..count])?;
                output.write_all(&buffer[..count])?;
            }
        }
        {
            let _time = super::metrics::measure(&metrics.hash);
            blocks.push(blake3::hash(&buffer[..count]).to_hex().to_string());
        }
        remaining -= count as u64;
        progress(count as u64, &path.join("/"));
    }
    {
        let _time = super::metrics::measure(&metrics.sync);
        output.sync_all()?;
    }
    bevy::log::info!(
        reflink = cloned,
        bytes = size,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Source snapshot ready"
    );
    metrics.report(&manifest.id);
    manifest.entries.push(Entry {
        path,
        directory: false,
        size,
        blocks,
    });
    Ok(())
}

pub(super) fn clone_snapshot(
    input: &mut File,
    output: &File,
    size: u64,
    control: &tokio::sync::watch::Receiver<super::Command>,
) -> anyhow::Result<bool> {
    #[cfg(not(windows))]
    let _ = control;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::fd::AsRawFd;
        if input.stream_position()? != 0
            || input.metadata()?.len() != size
            || !input.metadata()?.is_file()
        {
            return Ok(false);
        }
        // Linux FICLONE creates a COW snapshot, never a hard link to live source.
        // SAF subregions and unsupported/cross-filesystem sources use copying.
        const FICLONE: libc::c_ulong = 0x40049409;
        // SAFETY: both descriptors are owned/live; ioctl takes the source FD.
        if unsafe { libc::ioctl(output.as_raw_fd(), FICLONE as _, input.as_raw_fd()) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(
                libc::EOPNOTSUPP
                    | libc::ENOTTY
                    | libc::EXDEV
                    | libc::EINVAL
                    | libc::ENOSYS
                    | libc::EPERM
                    | libc::EACCES
            )
        ) {
            output.set_len(0)?;
            return Ok(false);
        }
        Err(error.into())
    }
    #[cfg(windows)]
    {
        Ok(crate::windows_io::clone_file(input, output, size, || {
            *control.borrow() == super::Command::Run
        })?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
    {
        let _ = (input, output, size);
        Ok(false)
    }
}

pub(super) struct Session {
    pub manifest: Manifest,
    pub root: PathBuf,
    pub journal: PathBuf,
    pub receipt: Mutex<Receipt>,
    pending: Mutex<Pending>,
    checkpoint: Mutex<()>,
    pub sync_requested: tokio::sync::Notify,
    metrics: super::metrics::Metrics,
    _lock: File,
}
struct Pending {
    offsets: Vec<u64>,
    hashes: Vec<Vec<String>>,
    dirty: HashMap<usize, Arc<Payload>>,
    bytes: u64,
    last_sync: Instant,
}
pub(super) struct Payload {
    id: usize,
    file: Mutex<File>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.metrics.report(&self.manifest.id);
    }
}
impl Session {
    pub fn open(
        manifest: Manifest,
        token: String,
        destination: portalo_core::settings::ReceiveDestination,
    ) -> anyhow::Result<Arc<Self>> {
        Self::open_at(base().join("incoming"), manifest, token, destination)
    }
    pub fn open_at(
        state: PathBuf,
        manifest: Manifest,
        token: String,
        destination: portalo_core::settings::ReceiveDestination,
    ) -> anyhow::Result<Arc<Self>> {
        manifest.validate()?;
        anyhow::ensure!(
            token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid resume token"
        );
        let state = state.join(&manifest.id);
        fs::create_dir_all(&state)?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state.join("lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("BUSY: task has an active session"))?;
        let journal = state.join("receipt.json");
        let mut receipt: Receipt = if journal.exists() {
            let receipt: Receipt = serde_json::from_slice(&fs::read(&journal)?)?;
            anyhow::ensure!(
                receipt.digest == manifest.digest()? && receipt.token == token,
                "Resume credentials or manifest mismatch"
            );
            receipt
        } else {
            let (directory, tree) = match destination {
                portalo_core::settings::ReceiveDestination::Directory(path) => (path, None),
                portalo_core::settings::ReceiveDestination::DocumentTree(tree) => {
                    (state.join("provider-staging"), Some(tree.uri))
                }
            };
            fs::create_dir_all(&directory)?;
            let target = directory.join(format!("{}-{}", manifest.name, &manifest.id[..8]));
            let receipt = Receipt {
                digest: manifest.digest()?,
                token,
                offsets: vec![0; manifest.entries.len()],
                result: None,
                target,
                select_storage: tree.is_some(),
                document_tree: tree,
                publishing: false,
                direct: None,
                hashes: if manifest.streaming {
                    vec![vec![]; manifest.entries.len()]
                } else {
                    vec![]
                },
            };
            save(&journal, &receipt)?;
            receipt
        };
        anyhow::ensure!(
            receipt.offsets.len() == manifest.entries.len(),
            "Corrupt receipt"
        );
        if manifest.streaming {
            anyhow::ensure!(
                receipt.hashes.len() == manifest.entries.len(),
                "Corrupt streaming receipt"
            );
            for (entry, hashes) in manifest.entries.iter().zip(&receipt.hashes) {
                anyhow::ensure!(
                    hashes.len() as u64 <= entry.size.div_ceil(BLOCK as u64),
                    "Invalid stored hash count"
                );
                anyhow::ensure!(
                    hashes.iter().all(|hash| hash.len() == 64
                        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())),
                    "Invalid stored hash"
                );
            }
        }
        save(&state.join("manifest.json"), &manifest)?;
        #[cfg(target_os = "android")]
        if receipt.result.is_none() && (receipt.select_storage || receipt.direct.is_some()) {
            let tree = receipt
                .document_tree
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Missing direct storage tree"))?;
            let descriptor = portalo_core::android_storage::prepare_transfer_target(
                tree,
                &state.to_string_lossy(),
                &serde_json::to_string(&manifest)?,
            )?;
            receipt.direct = serde_json::from_str(&descriptor)?;
            receipt.select_storage = false;
            save(&journal, &receipt)?;
        }
        #[cfg(not(target_os = "android"))]
        {
            receipt.select_storage = false;
        }
        let root = receipt
            .target
            .parent()
            .unwrap()
            .join(format!(".portalo-{}.part", manifest.id));
        let session = Arc::new(Self {
            manifest,
            root,
            journal,
            pending: Mutex::new(Pending {
                offsets: receipt.offsets.clone(),
                hashes: receipt.hashes.clone(),
                dirty: HashMap::new(),
                bytes: 0,
                last_sync: Instant::now(),
            }),
            receipt: Mutex::new(receipt),
            checkpoint: Mutex::new(()),
            sync_requested: tokio::sync::Notify::new(),
            metrics: super::metrics::Metrics::default(),
            _lock: lock,
        });
        session.recover()?;
        session.pending.lock().unwrap().offsets = session.receipt.lock().unwrap().offsets.clone();
        session.pending.lock().unwrap().hashes = session.receipt.lock().unwrap().hashes.clone();
        Ok(session)
    }
    fn recover(&self) -> anyhow::Result<()> {
        let _time = super::metrics::measure(&self.metrics.recover);
        let mut receipt = self.receipt.lock().unwrap();
        if receipt.result.is_some() {
            if (receipt.document_tree.is_some() || self.manifest.layout == super::Layout::File)
                && self.root.exists()
            {
                fs::remove_dir_all(&self.root)?;
            }
            return Ok(());
        }
        #[cfg(target_os = "android")]
        if let Some(direct) = &receipt.direct {
            anyhow::ensure!(
                direct.uris.len() == self.manifest.entries.len(),
                "Invalid direct document map"
            );
            if direct.published {
                self.verify_direct(direct, &receipt.hashes)?;
                let result = direct.root.clone();
                receipt.result = Some(result);
                receipt.offsets = self
                    .manifest
                    .entries
                    .iter()
                    .map(|entry| entry.size)
                    .collect();
                save(&self.journal, &*receipt)?;
                return Ok(());
            }
        }
        if receipt.publishing && receipt.document_tree.is_none() && self.published(&receipt)? {
            receipt.result = Some(receipt.target.display().to_string());
            save(&self.journal, &*receipt)?;
            if self.manifest.layout == super::Layout::File && self.root.exists() {
                fs::remove_dir_all(&self.root)?;
            }
            return Ok(());
        }
        if receipt.direct.is_none() {
            fs::create_dir_all(&self.root)?;
        }
        let mut buffer = vec![0; BLOCK];
        for (id, entry) in self.manifest.entries.iter().enumerate() {
            let path = self.manifest.path(&self.root, id);
            if entry.directory {
                if receipt.direct.is_none() {
                    fs::create_dir_all(path)?;
                }
                continue;
            }
            if receipt.direct.is_none()
                && let Some(parent) = path.parent()
            {
                fs::create_dir_all(parent)?;
            }
            let mut file = if let Some(direct) = &receipt.direct {
                Self::direct_file(&direct.uris[id], true)?
            } else {
                File::options()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(path)?
            };
            let expected = receipt.offsets[id].min(entry.size);
            let mut valid = 0;
            let hashes = if self.manifest.streaming {
                &receipt.hashes[id]
            } else {
                &entry.blocks
            };
            for hash in hashes {
                let count = (entry.size - valid).min(BLOCK as u64) as usize;
                if valid + count as u64 > expected
                    || file.read_exact(&mut buffer[..count]).is_err()
                    || blake3::hash(&buffer[..count]).to_hex().as_str() != hash
                {
                    break;
                }
                valid += count as u64;
            }
            file.set_len(valid)?;
            file.sync_all()?;
            receipt.offsets[id] = valid;
            if self.manifest.streaming {
                receipt.hashes[id].truncate(valid.div_ceil(BLOCK as u64) as usize);
            }
        }
        save(&self.journal, &*receipt)?;
        Ok(())
    }
    #[cfg(test)]
    pub fn block(&self, id: usize, offset: u64, bytes: &[u8]) -> anyhow::Result<u64> {
        let payload = self.open_payload(id)?;
        let result = self.write_block(&payload, offset, bytes)?;
        if self.sync_due() {
            self.flush()?;
        }
        Ok(result)
    }
    pub fn open_payload(&self, id: usize) -> anyhow::Result<Arc<Payload>> {
        Ok(Arc::new(Payload {
            id,
            file: Mutex::new(self.payload_file(id)?),
        }))
    }
    pub fn write_block(
        &self,
        payload: &Arc<Payload>,
        offset: u64,
        bytes: &[u8],
    ) -> anyhow::Result<u64> {
        self.write_block_checked(payload, offset, bytes, None)
    }
    pub fn write_stream_block(
        &self,
        payload: &Arc<Payload>,
        offset: u64,
        bytes: &[u8],
        hash: &str,
    ) -> anyhow::Result<u64> {
        self.write_block_checked(payload, offset, bytes, Some(hash))
    }
    fn write_block_checked(
        &self,
        payload: &Arc<Payload>,
        offset: u64,
        bytes: &[u8],
        hash: Option<&str>,
    ) -> anyhow::Result<u64> {
        let id = payload.id;
        let entry = &self.manifest.entries[id];
        anyhow::ensure!(
            offset < entry.size && offset.is_multiple_of(BLOCK as u64),
            "Invalid block offset"
        );
        anyhow::ensure!(
            bytes.len() as u64 == (entry.size - offset).min(BLOCK as u64),
            "Invalid block length"
        );
        let hash_time = super::metrics::measure(&self.metrics.hash);
        anyhow::ensure!(
            hash.is_some() == self.manifest.streaming,
            "Unexpected block hash mode"
        );
        let expected = if let Some(hash) = hash {
            hash
        } else {
            &entry.blocks[(offset / BLOCK as u64) as usize]
        };
        anyhow::ensure!(
            blake3::hash(bytes).to_hex().as_str() == expected,
            "CHECKSUM: block mismatch"
        );
        drop(hash_time);
        let mut file = payload.file.lock().unwrap();
        anyhow::ensure!(
            self.pending.lock().unwrap().offsets[id] == offset,
            "Out of order block"
        );
        let write_time = super::metrics::measure(&self.metrics.write);
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        drop(write_time);
        let mut pending = self.pending.lock().unwrap();
        if let Some(hash) = hash {
            anyhow::ensure!(
                pending.hashes[id].len() as u64 == offset / BLOCK as u64,
                "Out of order hash"
            );
            pending.hashes[id].push(hash.to_owned());
        }
        pending.offsets[id] = offset + bytes.len() as u64;
        pending.dirty.insert(id, payload.clone());
        pending.bytes += bytes.len() as u64;
        if pending.bytes >= 32 * 1024 * 1024
            || pending.last_sync.elapsed() >= Duration::from_secs(1)
        {
            self.sync_requested.notify_one();
        }
        Ok(pending.offsets[id])
    }
    fn payload_file(&self, id: usize) -> anyhow::Result<File> {
        let uri = self
            .receipt
            .lock()
            .unwrap()
            .direct
            .as_ref()
            .map(|direct| direct.uris[id].clone());
        if let Some(uri) = uri {
            return Self::direct_file(&uri, true);
        }
        Ok(File::options()
            .read(true)
            .write(true)
            .open(self.manifest.path(&self.root, id))?)
    }
    fn direct_file(uri: &str, write: bool) -> anyhow::Result<File> {
        #[cfg(target_os = "android")]
        {
            Ok(portalo_core::android_storage::open_transfer_document(
                uri, write,
            )?)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (uri, write);
            anyhow::bail!("SAF unavailable on this platform")
        }
    }
    pub fn flush(&self) -> anyhow::Result<()> {
        // Serialize journal generations, not file writes. Snapshot only fully
        // written offsets, then release the global state lock during fsync.
        let _checkpoint = self.checkpoint.lock().unwrap();
        let (offsets, dirty, bytes, hashes) = {
            let pending = self.pending.lock().unwrap();
            if pending.dirty.is_empty() {
                return Ok(());
            }
            (
                pending.offsets.clone(),
                pending.dirty.clone(),
                pending.bytes,
                pending.hashes.clone(),
            )
        };
        let _time = super::metrics::measure(&self.metrics.sync);
        for payload in dirty.values() {
            payload.file.lock().unwrap().sync_all()?;
        }
        let mut next = self.receipt.lock().unwrap().clone();
        next.offsets = offsets.clone();
        next.hashes = hashes;
        save(&self.journal, &next)?;
        *self.receipt.lock().unwrap() = next;
        let mut pending = self.pending.lock().unwrap();
        for &id in dirty.keys() {
            // Writes performed after our snapshot remain dirty for the next
            // generation, even if this fsync happened to include their bytes.
            if pending.offsets[id] == offsets[id] {
                pending.dirty.remove(&id);
            }
        }
        pending.bytes = pending.bytes.saturating_sub(bytes);
        pending.last_sync = Instant::now();
        Ok(())
    }
    #[cfg(test)]
    fn sync_due(&self) -> bool {
        let pending = self.pending.lock().unwrap();
        !pending.dirty.is_empty()
            && (pending.bytes >= 32 * 1024 * 1024
                || pending.last_sync.elapsed() >= Duration::from_secs(1))
    }
    pub fn commit(&self) -> anyhow::Result<String> {
        self.flush()?;
        let _time = super::metrics::measure(&self.metrics.publish);
        let mut receipt = self.receipt.lock().unwrap();
        if let Some(result) = &receipt.result {
            return Ok(result.clone());
        }
        anyhow::ensure!(
            self.manifest
                .entries
                .iter()
                .enumerate()
                .all(|(i, e)| e.directory || e.size == receipt.offsets[i]),
            "Incomplete task"
        );
        #[cfg(unix)]
        if receipt.direct.is_none() {
            for (index, entry) in self.manifest.entries.iter().enumerate().rev() {
                if entry.directory {
                    File::open(self.manifest.path(&self.root, index))?.sync_all()?;
                }
            }
            File::open(&self.root)?.sync_all()?;
        }
        let result = if receipt.direct.is_some() {
            #[cfg(target_os = "android")]
            {
                portalo_core::android_storage::publish_transfer_target(
                    &self.journal.parent().unwrap().to_string_lossy(),
                )?
            }
            #[cfg(not(target_os = "android"))]
            {
                anyhow::bail!("SAF unavailable on this platform")
            }
        } else if let Some(tree) = &receipt.document_tree {
            #[cfg(target_os = "android")]
            {
                save(
                    &self.root.join(".portalo-publication.json"),
                    &serde_json::json!({
                        "layout": self.manifest.layout,
                        "name": self.manifest.name,
                        "total": self.manifest.total()
                    }),
                )?;
                portalo_core::android_storage::publish_transfer(
                    tree,
                    &self.root.to_string_lossy(),
                    &self.manifest.name,
                    &self.manifest.id,
                )?
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = tree;
                anyhow::bail!("SAF unavailable on this platform");
            }
        } else {
            if self.manifest.layout == super::Layout::Legacy {
                receipt.publishing = true;
                save(&self.journal, &*receipt)?;
                publish(&self.root, &receipt.target)?;
            } else {
                let parent = receipt.target.parent().unwrap().to_owned();
                fs::create_dir_all(base())?;
                let lock = File::options()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(base().join("publication.lock"))?;
                FileExt::lock_exclusive(&lock)?;
                let single = self.manifest.layout == super::Layout::File;
                let mut number = 0u64;
                loop {
                    let name = if number == 0 {
                        self.manifest.name.clone()
                    } else {
                        format!("{}.{}", self.manifest.name, number)
                    };
                    let target = parent.join(name);
                    number += 1;
                    if target.try_exists()? {
                        continue;
                    }
                    receipt.target = target;
                    receipt.publishing = true;
                    save(&self.journal, &*receipt)?;
                    let result = if single {
                        fs::hard_link(self.manifest.path(&self.root, 0), &receipt.target)
                            .map_err(anyhow::Error::from)
                    } else {
                        publish(&self.root, &receipt.target)
                    };
                    match result {
                        Ok(()) => break,
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
                        {
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            #[cfg(unix)]
            File::open(receipt.target.parent().unwrap())?.sync_all()?;
            receipt.target.display().to_string()
        };
        receipt.result = Some(result.clone());
        save(&self.journal, &*receipt)?;
        if (receipt.document_tree.is_some() || self.manifest.layout == super::Layout::File)
            && self.root.exists()
        {
            // The durable completion record is sufficient for lost-ACK recovery.
            let _ = fs::remove_dir_all(&self.root);
        }
        Ok(result)
    }
    fn published(&self, receipt: &Receipt) -> anyhow::Result<bool> {
        if !receipt.target.try_exists()? {
            return Ok(false);
        }
        if self.manifest.layout == super::Layout::File {
            let source = self.manifest.path(&self.root, 0);
            return Ok(source.exists() && same_file::is_same_file(source, &receipt.target)?);
        }
        if self.root.exists() {
            return Ok(false);
        }
        self.verify_local(&receipt.target, &receipt.hashes)?;
        Ok(true)
    }
    fn verify_local(&self, root: &Path, hashes: &[Vec<String>]) -> anyhow::Result<()> {
        let _time = super::metrics::measure(&self.metrics.hash);
        anyhow::ensure!(
            fs::symlink_metadata(root)?.is_dir(),
            "Publication is not a directory"
        );
        let mut actual = HashSet::new();
        inventory(root, &[], &mut actual, self.manifest.entries.len())?;
        let expected: HashSet<_> = self
            .manifest
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        anyhow::ensure!(
            actual == expected,
            "Published directory structure does not match task manifest"
        );
        let mut buffer = vec![0; BLOCK];
        for (id, entry) in self.manifest.entries.iter().enumerate() {
            let path = self.manifest.path(root, id);
            let meta = fs::symlink_metadata(&path)?;
            anyhow::ensure!(
                meta.is_dir() == entry.directory && !meta.file_type().is_symlink(),
                "Published entry type mismatch: {}",
                path.display()
            );
            if !entry.directory {
                verify_file(
                    &mut File::open(path)?,
                    entry,
                    if self.manifest.streaming {
                        &hashes[id]
                    } else {
                        &entry.blocks
                    },
                    &mut buffer,
                )?;
            }
        }
        Ok(())
    }
    #[cfg(target_os = "android")]
    fn verify_direct(
        &self,
        target: &super::model::DirectTarget,
        hashes: &[Vec<String>],
    ) -> anyhow::Result<()> {
        // Java has already checked the complete path/type inventory. Re-open
        // every file read-only and verify BLAKE3 before claiming a renamed root.
        let _time = super::metrics::measure(&self.metrics.hash);
        let mut buffer = vec![0; BLOCK];
        for (id, entry) in self.manifest.entries.iter().enumerate() {
            if !entry.directory {
                verify_file(
                    &mut Self::direct_file(&target.uris[id], false)?,
                    entry,
                    if self.manifest.streaming {
                        &hashes[id]
                    } else {
                        &entry.blocks
                    },
                    &mut buffer,
                )?;
            }
        }
        Ok(())
    }
    pub fn cancel(&self) -> anyhow::Result<()> {
        let _checkpoint = self.checkpoint.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();
        let mut receipt = self.receipt.lock().unwrap();
        if receipt.result.is_some() {
            return Ok(());
        }
        // Workers have stopped before cancellation; release cached handles
        // before removing temporary files (required by Windows/providers).
        pending.dirty.clear();
        #[cfg(target_os = "android")]
        if receipt.direct.is_some() {
            portalo_core::android_storage::cancel_transfer_target(
                &self.journal.parent().unwrap().to_string_lossy(),
            )?;
            receipt.direct = None;
            receipt.select_storage = true;
        } else if receipt.document_tree.is_some() && self.root.exists() {
            portalo_core::android_storage::cancel_transfer(&self.root.to_string_lossy())?;
        }
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        receipt.offsets.fill(0);
        for hashes in &mut receipt.hashes {
            hashes.clear();
        }
        save(&self.journal, &*receipt)?;
        pending.offsets.fill(0);
        for hashes in &mut pending.hashes {
            hashes.clear();
        }
        pending.dirty.clear();
        pending.bytes = 0;
        Ok(())
    }
}
fn verify_file(
    file: &mut File,
    entry: &Entry,
    hashes: &[String],
    buffer: &mut [u8],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        hashes.len() as u64 == entry.size.div_ceil(BLOCK as u64),
        "Incomplete publication hashes"
    );
    anyhow::ensure!(
        file.metadata()?.len() == entry.size,
        "Published file length mismatch: {}",
        entry.path.join("/")
    );
    for (index, hash) in hashes.iter().enumerate() {
        let length = (entry.size - index as u64 * BLOCK as u64).min(BLOCK as u64) as usize;
        file.read_exact(&mut buffer[..length])?;
        anyhow::ensure!(
            blake3::hash(&buffer[..length]).to_hex().as_str() == hash,
            "Published file checksum mismatch: {}",
            entry.path.join("/")
        );
    }
    Ok(())
}
fn inventory(
    root: &Path,
    relative: &[String],
    entries: &mut HashSet<Vec<String>>,
    max: usize,
) -> anyhow::Result<()> {
    for item in fs::read_dir(root)? {
        let item = item?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Non UTF-8 publication path"))?;
        let kind = item.file_type()?;
        anyhow::ensure!(
            !kind.is_symlink() && (kind.is_dir() || kind.is_file()),
            "Unsupported publication entry"
        );
        let mut path = relative.to_vec();
        path.push(name);
        anyhow::ensure!(
            path.len() <= 64 && entries.len() < max,
            "Unexpected publication entries"
        );
        entries.insert(path.clone());
        if kind.is_dir() {
            inventory(&item.path(), &path, entries, max)?;
        }
    }
    Ok(())
}
fn publish(source: &Path, target: &Path) -> anyhow::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let target = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        // SAFETY: both pointers are live NUL-terminated paths. No replacement.
        if unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        anyhow::ensure!(!target.exists(), "Destination already exists");
        fs::rename(source, target)?;
    }
    Ok(())
}
