use bevy::prelude::*;
use portalo_core::receive::{MAX_ACTIVE_RECEIVES, ReceiveTransfers};
use portalo_core::settings::{ReceiveDestination, SharedSettings};
use portalo_core::{ReceiveFeedback, TransferProgressMsg, TransferStatus};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime::Runtime,
    sync::{Semaphore, watch},
    time::timeout,
};

const MAGIC: &[u8; 8] = b"PORTALO1";
const ACK: &[u8; 8] = b"RECEIVED";
const IDLE: Duration = Duration::from_secs(30);
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

pub struct NetworkPlugin;
#[derive(Resource)]
pub struct TokioRuntime(pub Runtime);
#[derive(Resource)]
struct ReceiveNoticeSender(watch::Sender<String>);
#[cfg(target_os = "android")]
mod android_saf;
mod connect;
mod filename;
mod receive_progress;
mod source;
pub mod transfer;
#[cfg(windows)]
mod windows_io;
pub use source::SendSource;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod kernel_copy;
use receive_progress::ReceiveProgress;

impl Plugin for NetworkPlugin {
    fn build(&self, app: &mut App) {
        let (notice_tx, notice_rx) = watch::channel(String::new());
        app.init_resource::<SharedSettings>()
            .init_resource::<ReceiveTransfers>()
            .insert_resource(ReceiveNoticeSender(notice_tx))
            .insert_resource(ReceiveFeedback(notice_rx))
            .insert_resource(TokioRuntime(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create Tokio runtime"),
            ))
            .add_systems(Startup, setup_network_listener);
    }
}

fn buffer_size() -> usize {
    std::env::var("PORTALO_BUFFER_KIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| (64..=1024).contains(n))
        .unwrap_or(256)
        * 1024
}

fn setup_network_listener(
    runtime: Res<TokioRuntime>,
    settings: Res<SharedSettings>,
    notices: Res<ReceiveNoticeSender>,
    transfers: Res<ReceiveTransfers>,
) {
    let settings = settings.clone();
    let notices = notices.0.clone();
    let transfers = transfers.clone();
    runtime.0.spawn(async move {
        let result: anyhow::Result<()> = async {
            let listener = TcpListener::bind("0.0.0.0:49527").await?;
            let slots = Arc::new(Semaphore::new(MAX_ACTIVE_RECEIVES));
            loop {
                let (mut stream, addr) = listener.accept().await?;
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    transfer::busy(&mut stream).await;
                    continue;
                };
                let destination = settings
                    .0
                    .read()
                    .expect("Settings lock")
                    .receive_destination();
                let transfers = transfers.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) =
                        start_file_receiver_tracked(stream, destination, transfers).await
                    {
                        error!("Receive from {addr}: {error:#}");
                    }
                });
            }
        }
        .await;
        if let Err(error) = result {
            error!("Network listener: {error:#}");
            notices.send_replace(format!("无法接收文件：{error:#}"));
        }
    });
}

pub async fn start_file_receiver(stream: TcpStream, directory: PathBuf) -> anyhow::Result<()> {
    start_file_receiver_with_destination(stream, ReceiveDestination::Directory(directory)).await
}

pub async fn start_file_receiver_with_destination(
    stream: TcpStream,
    destination: ReceiveDestination,
) -> anyhow::Result<()> {
    receive_impl(stream, destination, None).await
}

pub async fn start_file_receiver_tracked(
    stream: TcpStream,
    destination: ReceiveDestination,
    transfers: ReceiveTransfers,
) -> anyhow::Result<()> {
    receive_impl(stream, destination, Some(transfers)).await
}

async fn receive_impl(
    mut stream: TcpStream,
    destination: ReceiveDestination,
    transfers: Option<ReceiveTransfers>,
) -> anyhow::Result<()> {
    let mut progress = ReceiveProgress::new(stream.peer_addr()?, transfers)?;
    let mut magic = [0; 8];
    timeout(IDLE, stream.read_exact(&mut magic)).await??;
    if &magic == transfer::MAGIC {
        return transfer::receive(stream, destination, progress).await;
    }
    anyhow::ensure!(&magic == MAGIC, "Unsupported protocol; update both devices");
    let result = receive_payload(&mut stream, destination, &mut progress).await;
    progress.finish(result.as_ref().err().map(|error| format!("{error:#}")));
    result
}

async fn receive_payload(
    stream: &mut TcpStream,
    destination: ReceiveDestination,
    progress: &mut ReceiveProgress,
) -> anyhow::Result<()> {
    let (name, size) = timeout(IDLE, async {
        let length = stream.read_u32_le().await? as usize;
        anyhow::ensure!((1..=4096).contains(&length), "Invalid filename length");
        let mut name = vec![0; length];
        stream.read_exact(&mut name).await?;
        let name = String::from_utf8(name)?;
        filename::validate(&name, cfg!(target_os = "windows"))?;
        let size = stream.read_u64_le().await?;
        Ok::<_, anyhow::Error>((name, size))
    })
    .await??;
    progress.header(&name, size);
    match destination {
        ReceiveDestination::Directory(directory) => {
            receive_local(stream, directory, &name, size, progress).await?
        }
        ReceiveDestination::DocumentTree(tree) => {
            #[cfg(target_os = "android")]
            android_saf::receive(stream, tree.uri, name.clone(), size, progress).await?;
            #[cfg(not(target_os = "android"))]
            {
                let _ = tree;
                anyhow::bail!("系统文档目录仅支持安卓");
            }
        }
    }
    progress.saved();
    timeout(IDLE, stream.write_all(ACK)).await??;
    info!("Received {name}: {size} bytes");
    Ok(())
}

async fn copy_exact(
    stream: &mut TcpStream,
    file: &mut File,
    size: u64,
    progress: &mut ReceiveProgress,
) -> anyhow::Result<()> {
    progress.receiving();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Cloud SAF providers may return a pipe rather than a regular file.
        // Preserve Tokio's timed streaming path for such descriptors.
        if file.metadata().await?.is_file() {
            let mut previous = 0;
            kernel_copy::copy(
                stream,
                file,
                size,
                kernel_copy::Direction::Receive,
                |current| {
                    progress.advance(current - previous);
                    previous = current;
                },
            )
            .await?;
        } else {
            copy_buffered_exact(stream, file, size, progress).await?;
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    copy_buffered_exact(stream, file, size, progress).await?;
    progress.saving();
    timeout(IDLE, file.flush()).await??;
    Ok(())
}

async fn copy_buffered_exact(
    stream: &mut TcpStream,
    file: &mut File,
    size: u64,
    progress: &mut ReceiveProgress,
) -> anyhow::Result<()> {
    if size == 0 {
        return Ok(());
    }
    let mut buffer = vec![0; buffer_size()];
    let mut remaining = size;
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = timeout(IDLE, stream.read(&mut buffer[..limit])).await??;
        anyhow::ensure!(count > 0, "Incomplete file: {remaining} bytes missing");
        timeout(IDLE, file.write_all(&buffer[..count])).await??;
        remaining -= count as u64;
        progress.advance(count as u64);
    }
    Ok(())
}

async fn receive_local(
    stream: &mut TcpStream,
    directory: PathBuf,
    name: &str,
    size: u64,
    progress: &mut ReceiveProgress,
) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(&directory).await?;
    let (temporary, mut file) = loop {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".portalo-{}-{id}.part", std::process::id()));
        match File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => break (path, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    // Own the temporary path before the first payload await, so dropping a
    // cancelled receive also unlinks its partial output.
    let temporary = tempfile::TempPath::try_from_path(temporary)?;
    copy_exact(stream, &mut file, size, progress).await?;
    drop(file);
    // Use each platform's no-replace rename, with tempfile's fallback for
    // older kernels. Publication neither copies the payload nor overwrites.
    let publish_name = name.to_owned();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut temporary = temporary;
        let mut suffix = 0u64;
        loop {
            let target = directory.join(if suffix == 0 {
                publish_name.clone()
            } else {
                format!("{publish_name}.{suffix}")
            });
            match temporary.persist_noclobber(&target) {
                Ok(()) => return Ok(()),
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    temporary = e.path;
                    suffix += 1;
                }
                Err(e) => {
                    // Android emulated storage may reject both NOREPLACE
                    // rename and hard links. Reserve a fresh filename first,
                    // then replace only our own empty reservation. No second
                    // payload copy is needed and existing names are skipped.
                    #[cfg(target_os = "android")]
                    if matches!(
                        e.error.kind(),
                        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                    ) {
                        temporary = e.path;
                        match reserve_and_rename(&temporary, &target) {
                            Ok(()) => return Ok(()),
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                suffix += 1;
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    return Err(e.error);
                }
            }
        }
    })
    .await??;
    Ok(())
}

#[cfg(any(target_os = "android", test))]
fn reserve_and_rename(source: &Path, target: &Path) -> std::io::Result<()> {
    let reservation = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    drop(reservation);
    if let Err(error) = std::fs::rename(source, target) {
        let _ = std::fs::remove_file(target);
        return Err(error);
    }
    Ok(())
}

pub async fn send_file_fast(
    addresses: Vec<SocketAddr>,
    file_path: impl AsRef<Path>,
    progress: watch::Sender<TransferProgressMsg>,
    id: u64,
) -> anyhow::Result<SocketAddr> {
    send_source_fast(
        addresses,
        SendSource::Path(file_path.as_ref().to_owned()),
        progress,
        id,
    )
    .await
}

pub async fn send_source_fast(
    addresses: Vec<SocketAddr>,
    source: SendSource,
    progress: watch::Sender<TransferProgressMsg>,
    id: u64,
) -> anyhow::Result<SocketAddr> {
    let mut snapshot = TransferProgressMsg {
        id,
        status: TransferStatus::Connecting,
        ..Default::default()
    };
    progress.send_replace(snapshot.clone());
    let result = send_inner(addresses, &source, &progress, &mut snapshot).await;
    snapshot.status = match &result {
        Ok(_) => TransferStatus::Complete,
        Err(e) => TransferStatus::Failed(format!("{e:#}")),
    };
    progress.send_replace(snapshot);
    result
}

async fn send_inner(
    addresses: Vec<SocketAddr>,
    source: &SendSource,
    progress: &watch::Sender<TransferProgressMsg>,
    snapshot: &mut TransferProgressMsg,
) -> anyhow::Result<SocketAddr> {
    let mut file = match source {
        SendSource::OpenFile { file, .. } => File::from_std(
            file.lock()
                .map_err(|_| anyhow::anyhow!("Source lock poisoned"))?
                .take()
                .ok_or_else(|| anyhow::anyhow!("Selected document already consumed"))?,
        ),
        _ => File::open(source.path().expect("Path source")).await?,
    };
    let metadata = file.metadata().await?;
    anyhow::ensure!(metadata.is_file(), "Select a regular file");
    snapshot.total = match source {
        SendSource::OpenFile { size, offset, .. } => {
            anyhow::ensure!(
                offset
                    .checked_add(*size)
                    .is_some_and(|end| end <= metadata.len()),
                "Source file shortened before transfer"
            );
            use tokio::io::AsyncSeekExt;
            file.seek(std::io::SeekFrom::Start(*offset)).await?;
            *size
        }
        _ => metadata.len(),
    };
    let name = source
        .name()
        .ok_or_else(|| anyhow::anyhow!("Invalid filename"))?;
    filename::validate(name, false)?;
    let connected = Instant::now();
    let (mut stream, address) = connect::connect(addresses).await?;
    stream.set_nodelay(true)?;
    let mut header = Vec::with_capacity(20 + name.len());
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&(name.len() as u32).to_le_bytes());
    header.extend_from_slice(name.as_bytes());
    header.extend_from_slice(&snapshot.total.to_le_bytes());
    timeout(IDLE, stream.write_all(&header)).await??;
    info!("Connected to {address} in {:?}", connected.elapsed());
    let started = Instant::now();
    snapshot.status = TransferStatus::Sending;
    progress.send_replace(snapshot.clone());
    #[cfg(any(target_os = "linux", target_os = "android"))]
    kernel_copy::copy(
        &stream,
        &file,
        snapshot.total,
        kernel_copy::Direction::Send,
        |current| {
            if snapshot.current != current {
                snapshot.current = current;
                progress.send_replace(snapshot.clone());
            }
        },
    )
    .await?;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let mut file = file;
        let mut reported = Instant::now();
        let mut buffer = vec![0; buffer_size()];
        while snapshot.current < snapshot.total {
            let limit = (snapshot.total - snapshot.current).min(buffer.len() as u64) as usize;
            let count = timeout(IDLE, file.read(&mut buffer[..limit])).await??;
            anyhow::ensure!(count > 0, "Source file shortened during transfer");
            timeout(IDLE, stream.write_all(&buffer[..count])).await??;
            snapshot.current += count as u64;
            if reported.elapsed() >= Duration::from_millis(100) {
                progress.send_replace(snapshot.clone());
                reported = Instant::now();
            }
        }
    }
    snapshot.status = TransferStatus::AwaitingConfirmation;
    progress.send_replace(snapshot.clone());
    let mut ack = [0; 8];
    timeout(IDLE, stream.read_exact(&mut ack)).await??;
    anyhow::ensure!(&ack == ACK, "Invalid receive confirmation");
    info!(
        "Confirmed {} bytes in {:?} ({:.2} MiB/s)",
        snapshot.total,
        started.elapsed(),
        snapshot.total as f64 / 1_048_576.0 / started.elapsed().as_secs_f64()
    );
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use portalo_core::receive::ReceivePhase;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test]
    async fn non_regular_provider_descriptor_uses_timed_streaming_fallback() {
        use std::{
            io::Read,
            os::{fd::OwnedFd, unix::net::UnixStream},
        };
        let (mut provider, output) = UnixStream::pair().unwrap();
        let mut file = File::from_std(std::fs::File::from(OwnedFd::from(output)));
        assert!(!file.metadata().await.unwrap().is_file());
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            provider.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let receiver = tokio::spawn(async move {
            let (mut stream, peer) = listener.accept().await.unwrap();
            let mut progress = ReceiveProgress::new(peer, None).unwrap();
            copy_exact(&mut stream, &mut file, 256 * 1024, &mut progress)
                .await
                .unwrap();
        });
        let data = vec![37; 256 * 1024];
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(&data).await.unwrap();
        receiver.await.unwrap();
        assert_eq!(reader.join().unwrap(), data);
    }

    #[tokio::test]
    async fn owned_document_uses_logical_name_length_and_can_only_be_consumed_once() {
        use std::io::{Seek, Write};
        let target = tempfile::tempdir().unwrap();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"prefix-payload-extra").unwrap();
        file.seek(std::io::SeekFrom::Start(3)).unwrap();
        let source = SendSource::from_file_region(file, "文档.bin".into(), 7, 7);
        let consumed = source.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let directory = target.path().to_owned();
        let receiver = tokio::spawn(async move {
            start_file_receiver(listener.accept().await.unwrap().0, directory).await
        });
        let (progress, _) = watch::channel(TransferProgressMsg::default());
        send_source_fast(vec![address], source, progress.clone(), 1)
            .await
            .unwrap();
        receiver.await.unwrap().unwrap();
        assert_eq!(
            std::fs::read(target.path().join("文档.bin")).unwrap(),
            b"payload"
        );
        assert!(
            send_source_fast(vec![address], consumed, progress, 2)
                .await
                .unwrap_err()
                .to_string()
                .contains("already consumed")
        );
    }

    #[tokio::test]
    async fn document_region_bounds_are_checked_before_connecting() {
        for (offset, size) in [(5, 1), (u64::MAX, 2)] {
            let file = tempfile::tempfile().unwrap();
            file.set_len(5).unwrap();
            let source = SendSource::from_file_region(file, "region.bin".into(), offset, size);
            let (progress, _) = watch::channel(TransferProgressMsg::default());
            let error = send_source_fast(vec![], source, progress, 1)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("Source file shortened"));
        }
    }

    #[tokio::test]
    async fn cancelling_receive_removes_partial_file_and_releases_socket() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let ledger = ReceiveTransfers::default();
        let tracked = ledger.clone();
        let target = directory.path().to_owned();
        let receiver = tokio::spawn(async move {
            start_file_receiver_tracked(
                listener.accept().await.unwrap().0,
                ReceiveDestination::Directory(target),
                tracked,
            )
            .await
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(MAGIC).await.unwrap();
        stream.write_u32_le(8).await.unwrap();
        stream.write_all(b"test.bin").await.unwrap();
        stream.write_u64_le(1024 * 1024).await.unwrap();
        stream.write_all(&[7; 1024]).await.unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if ledger
                    .snapshot_since(None)
                    .unwrap()
                    .1
                    .first()
                    .is_some_and(|task| task.received == 1024)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        receiver.abort();
        let _ = receiver.await;
        let mut byte = [0];
        let closed = timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0)) || closed.is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        assert!(matches!(
            ledger.snapshot_since(None).unwrap().1[0].phase,
            ReceivePhase::Failed(_)
        ));
    }

    #[tokio::test]
    async fn tracked_receive_keeps_partial_progress_then_reports_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let ledger = ReceiveTransfers::default();
        let tracked = ledger.clone();
        let target = directory.path().to_path_buf();
        let receiver = tokio::spawn(async move {
            start_file_receiver_tracked(
                listener.accept().await.unwrap().0,
                ReceiveDestination::Directory(target),
                tracked,
            )
            .await
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(MAGIC).await.unwrap();
        stream.write_u32_le(8).await.unwrap();
        stream.write_all(b"test.bin").await.unwrap();
        stream.write_u64_le(1024 * 1024).await.unwrap();
        stream.write_all(&vec![7; 128 * 1024]).await.unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                let (_, tasks) = ledger.snapshot_since(None).unwrap();
                if tasks
                    .first()
                    .is_some_and(|task| task.received == 128 * 1024)
                {
                    assert_eq!(tasks[0].file_name, "test.bin");
                    assert_eq!(tasks[0].phase, ReceivePhase::Receiving);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stream.shutdown().await.unwrap();
        assert!(receiver.await.unwrap().is_err());
        let tasks = ledger.snapshot_since(None).unwrap().1;
        assert_eq!(tasks[0].received, 128 * 1024);
        assert!(matches!(tasks[0].phase, ReceivePhase::Failed(_)));
        assert!(tasks[0].saved_at.is_none());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn tracked_empty_file_completes_with_source_and_correct_terminal_state() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let path = source.path().join("empty.bin");
        tokio::fs::write(&path, []).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let ledger = ReceiveTransfers::default();
        let tracked = ledger.clone();
        let directory = target.path().to_path_buf();
        let receiver = tokio::spawn(async move {
            start_file_receiver_tracked(
                listener.accept().await.unwrap().0,
                ReceiveDestination::Directory(directory),
                tracked,
            )
            .await
        });
        let (tx, _) = watch::channel(TransferProgressMsg::default());
        send_file_fast(vec![address], path, tx, 1).await.unwrap();
        receiver.await.unwrap().unwrap();
        let task = ledger.snapshot_since(None).unwrap().1.remove(0);
        assert_eq!(task.phase, ReceivePhase::Completed);
        assert_eq!(task.progress(), 1.0);
        assert_eq!(task.total, Some(0));
        assert!(task.saved_at.is_some());
        assert!(task.source.ip().is_loopback());
    }

    #[test]
    fn reserved_publication_preserves_existing_files_and_cleans_failed_reservations() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.part");
        let target = directory.path().join("target.bin");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&target, b"original").unwrap();
        assert_eq!(
            reserve_and_rename(&source, &target).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        let new_target = directory.path().join("new.bin");
        reserve_and_rename(&source, &new_target).unwrap();
        assert_eq!(std::fs::read(&new_target).unwrap(), b"new");
        let missing_target = directory.path().join("missing.bin");
        assert!(reserve_and_rename(&source, &missing_target).is_err());
        assert!(!missing_target.exists());
    }

    async fn receiver(
        directory: PathBuf,
    ) -> (SocketAddr, tokio::task::JoinHandle<anyhow::Result<()>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            start_file_receiver(listener.accept().await?.0, directory).await
        });
        (address, task)
    }

    #[tokio::test]
    async fn round_trip_empty_large_and_collision() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let (tx, rx) = watch::channel(TransferProgressMsg::default());
        for (id, size) in [(1, 0), (2, 37), (3, 2 * 1024 * 1024 + 17)] {
            let bytes: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
            let path = source.path().join("test.bin");
            tokio::fs::write(&path, &bytes).await.unwrap();
            let (addr, task) = receiver(target.path().into()).await;
            send_file_fast(vec![addr], &path, tx.clone(), id)
                .await
                .unwrap();
            task.await.unwrap().unwrap();
            let name = if id == 1 {
                "test.bin".to_owned()
            } else {
                format!("test.bin.{}", id - 1)
            };
            assert_eq!(
                tokio::fs::read(target.path().join(name)).await.unwrap(),
                bytes
            );
            let state = rx.borrow();
            assert_eq!(state.id, id);
            assert_eq!(state.current, size as u64);
            assert_eq!(state.status, TransferStatus::Complete);
        }
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 3);
    }

    #[tokio::test]
    async fn truncated_transfer_is_not_published() {
        let directory = tempfile::tempdir().unwrap();
        let (addr, task) = receiver(directory.path().into()).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(MAGIC).await.unwrap();
        stream.write_u32_le(8).await.unwrap();
        stream.write_all(b"test.bin").await.unwrap();
        stream.write_u64_le(100).await.unwrap();
        stream.write_all(b"partial").await.unwrap();
        stream.shutdown().await.unwrap();
        assert!(task.await.unwrap().is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn missing_ack_is_failure_and_address_fallback_works() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty");
        tokio::fs::write(&path, []).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = [0; 25];
            stream.read_exact(&mut header).await.unwrap();
        });
        let (tx, rx) = watch::channel(TransferProgressMsg::default());
        assert!(
            send_file_fast(vec!["127.0.0.1:0".parse().unwrap(), addr], path, tx, 1)
                .await
                .is_err()
        );
        task.await.unwrap();
        assert!(matches!(rx.borrow().status, TransferStatus::Failed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn incomplete_header_times_out() {
        let directory = tempfile::tempdir().unwrap();
        let (addr, task) = receiver(directory.path().into()).await;
        let _stream = TcpStream::connect(addr).await.unwrap();
        assert!(task.await.unwrap().is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn concurrent_same_name_does_not_overwrite() {
        let target = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let a = first.path().join("same.bin");
        let b = second.path().join("same.bin");
        tokio::fs::write(&a, b"first").await.unwrap();
        tokio::fs::write(&b, b"second").await.unwrap();
        let (addr_a, task_a) = receiver(target.path().into()).await;
        let (addr_b, task_b) = receiver(target.path().into()).await;
        let (tx_a, _) = watch::channel(TransferProgressMsg::default());
        let (tx_b, _) = watch::channel(TransferProgressMsg::default());
        let (sent_a, sent_b) = tokio::join!(
            send_file_fast(vec![addr_a], a, tx_a, 1),
            send_file_fast(vec![addr_b], b, tx_b, 2)
        );
        sent_a.unwrap();
        sent_b.unwrap();
        task_a.await.unwrap().unwrap();
        task_b.await.unwrap().unwrap();
        let mut contents = vec![
            tokio::fs::read(target.path().join("same.bin"))
                .await
                .unwrap(),
            tokio::fs::read(target.path().join("same.bin.1"))
                .await
                .unwrap(),
        ];
        contents.sort();
        assert_eq!(contents, vec![b"first".to_vec(), b"second".to_vec()]);
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 2);
    }
}
