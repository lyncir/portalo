use bevy::prelude::*;
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

impl Plugin for NetworkPlugin {
    fn build(&self, app: &mut App) {
        let (notice_tx, notice_rx) = watch::channel(String::new());
        app.init_resource::<SharedSettings>()
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
) {
    let settings = settings.clone();
    let notices = notices.0.clone();
    runtime.0.spawn(async move {
        let result: anyhow::Result<()> = async {
            let listener = TcpListener::bind("0.0.0.0:49527").await?;
            let slots = Arc::new(Semaphore::new(2));
            loop {
                let (stream, addr) = listener.accept().await?;
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let destination = settings
                    .0
                    .read()
                    .expect("Settings lock")
                    .receive_destination();
                let notices = notices.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match start_file_receiver_with_destination(stream, destination).await {
                        Ok(()) => {
                            notices.send_replace("文件接收完成。".into());
                        }
                        Err(error) => {
                            error!("Receive from {addr}: {error:#}");
                            notices.send_replace(format!("接收失败：{error:#}"));
                        }
                    }
                });
            }
        }
        .await;
        if let Err(error) = result {
            error!("Network listener: {error:#}");
        }
    });
}

pub async fn start_file_receiver(stream: TcpStream, directory: PathBuf) -> anyhow::Result<()> {
    start_file_receiver_with_destination(stream, ReceiveDestination::Directory(directory)).await
}

pub async fn start_file_receiver_with_destination(
    mut stream: TcpStream,
    destination: ReceiveDestination,
) -> anyhow::Result<()> {
    let (name, size) = timeout(IDLE, async {
        let mut magic = [0; 8];
        stream.read_exact(&mut magic).await?;
        anyhow::ensure!(&magic == MAGIC, "Unsupported protocol; update both devices");
        let length = stream.read_u32_le().await? as usize;
        anyhow::ensure!((1..=4096).contains(&length), "Invalid filename length");
        let mut name = vec![0; length];
        stream.read_exact(&mut name).await?;
        let name = String::from_utf8(name)?;
        anyhow::ensure!(
            name != "." && name != ".." && !name.contains(['/', '\\', '\0']),
            "Invalid filename"
        );
        let size = stream.read_u64_le().await?;
        Ok::<_, anyhow::Error>((name, size))
    })
    .await??;
    match destination {
        ReceiveDestination::Directory(directory) => {
            receive_local(&mut stream, directory, &name, size).await?
        }
        ReceiveDestination::DocumentTree(tree) => {
            #[cfg(target_os = "android")]
            android_saf::receive(&mut stream, tree.uri, name.clone(), size).await?;
            #[cfg(not(target_os = "android"))]
            {
                let _ = tree;
                anyhow::bail!("系统文档目录仅支持安卓");
            }
        }
    }
    timeout(IDLE, stream.write_all(ACK)).await??;
    info!("Received {name}: {size} bytes");
    Ok(())
}

async fn copy_exact(stream: &mut TcpStream, file: &mut File, size: u64) -> anyhow::Result<()> {
    let mut buffer = vec![0; buffer_size()];
    let mut remaining = size;
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = timeout(IDLE, stream.read(&mut buffer[..limit])).await??;
        anyhow::ensure!(count > 0, "Incomplete file: {remaining} bytes missing");
        timeout(IDLE, file.write_all(&buffer[..count])).await??;
        remaining -= count as u64;
    }
    timeout(IDLE, file.flush()).await??;
    Ok(())
}

async fn receive_local(
    stream: &mut TcpStream,
    directory: PathBuf,
    name: &str,
    size: u64,
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
    let result: anyhow::Result<()> = async {
        copy_exact(stream, &mut file, size).await?;
        drop(file);
        // Use each platform's no-replace rename, with tempfile's fallback for
        // older kernels. Publication neither copies the payload nor overwrites.
        let publish_path = temporary.clone();
        let publish_name = name.to_owned();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut temporary = tempfile::TempPath::try_from_path(publish_path)?;
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
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
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
    let mut snapshot = TransferProgressMsg {
        id,
        status: TransferStatus::Connecting,
        ..Default::default()
    };
    progress.send_replace(snapshot.clone());
    let result = send_inner(addresses, file_path.as_ref(), &progress, &mut snapshot).await;
    snapshot.status = match &result {
        Ok(_) => TransferStatus::Complete,
        Err(e) => TransferStatus::Failed(format!("{e:#}")),
    };
    progress.send_replace(snapshot);
    result
}

async fn send_inner(
    addresses: Vec<SocketAddr>,
    path: &Path,
    progress: &watch::Sender<TransferProgressMsg>,
    snapshot: &mut TransferProgressMsg,
) -> anyhow::Result<SocketAddr> {
    let mut file = File::open(path).await?;
    let metadata = file.metadata().await?;
    anyhow::ensure!(metadata.is_file(), "Select a regular file");
    snapshot.total = metadata.len();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid filename"))?;
    anyhow::ensure!(name.len() <= 4096, "Filename too long");
    let connected = Instant::now();
    let mut connection = None;
    for address in addresses {
        if let Ok(Ok(stream)) = timeout(Duration::from_secs(2), TcpStream::connect(address)).await {
            connection = Some((stream, address));
            break;
        }
        anyhow::ensure!(
            connected.elapsed() < Duration::from_secs(10),
            "Connection deadline exceeded"
        );
    }
    let (mut stream, address) =
        connection.ok_or_else(|| anyhow::anyhow!("No reachable device address"))?;
    stream.set_nodelay(true)?;
    let mut header = Vec::with_capacity(20 + name.len());
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&(name.len() as u32).to_le_bytes());
    header.extend_from_slice(name.as_bytes());
    header.extend_from_slice(&snapshot.total.to_le_bytes());
    timeout(IDLE, stream.write_all(&header)).await??;
    info!("Connected to {address} in {:?}", connected.elapsed());
    let started = Instant::now();
    let mut reported = Instant::now();
    snapshot.status = TransferStatus::Sending;
    progress.send_replace(snapshot.clone());
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
