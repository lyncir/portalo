use super::{
    BLOCK, Command, FRAME, STREAMS, SavedTask,
    storage::{self, Session},
    wire::{self, Message},
};
use portalo_core::{TransferProgressMsg, TransferStatus};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::{TcpStream, tcp::OwnedReadHalf},
    sync::{mpsc, watch},
    task::JoinSet,
};

struct Reader(tokio::task::JoinHandle<()>);
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct CaptureAhead {
    _worker: Reader,
    consumed: watch::Sender<u64>,
    ready: watch::Receiver<Result<u64, String>>,
}
impl CaptureAhead {
    async fn start(
        path: std::path::PathBuf,
        size: u64,
        offset: u64,
        captures: Arc<tokio::sync::RwLock<()>>,
    ) -> Self {
        let (consumed, mut credit) = watch::channel(offset);
        let (progress, ready) = watch::channel(Ok(offset));
        // The producer itself and each uninterruptible capture both participate
        // in sender draining, including errors and aborted async file workers.
        let lifetime = captures.clone().read_owned().await;
        let worker = Reader(tokio::spawn(async move {
            let _lifetime = lifetime;
            let mut next = offset;
            while next < size {
                while next.saturating_sub(*credit.borrow()) >= 32 * 1024 * 1024 {
                    if credit.changed().await.is_err() {
                        return;
                    }
                }
                if credit.has_changed().is_err() {
                    return;
                }
                let path = path.clone();
                let guard = captures.clone().read_owned().await;
                let result = tokio::task::spawn_blocking(move || {
                    let _guard = guard;
                    super::preparation::capture_ahead(&path, size, next)
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
                match result {
                    Ok(end) if end > next => {
                        next = end;
                        let _ = progress.send_replace(Ok(end));
                    }
                    Ok(_) => {
                        let _ = progress
                            .send_replace(Err("Snapshot preparation made no progress".into()));
                        return;
                    }
                    Err(error) => {
                        let _ = progress.send_replace(Err(format!("{error:#}")));
                        return;
                    }
                }
            }
        }));
        Self {
            _worker: worker,
            consumed,
            ready,
        }
    }
    async fn wait(&mut self, end: u64) -> anyhow::Result<()> {
        loop {
            match self.ready.borrow_and_update().clone() {
                Ok(ready) if ready >= end => return Ok(()),
                Err(error) => anyhow::bail!(error),
                _ => {}
            }
            self.ready
                .changed()
                .await
                .map_err(|_| anyhow::anyhow!("Snapshot preparation stopped"))?;
        }
    }
}
type Packet = (Message, Vec<u8>);
struct SendTimings {
    path: std::path::PathBuf,
    offset: u64,
    started: std::time::Instant,
    capture: std::time::Duration,
    read: std::time::Duration,
    queue: std::time::Duration,
    ack: std::time::Duration,
}
impl Drop for SendTimings {
    fn drop(&mut self) {
        bevy::log::info!(path=%self.path.display(), resumed_at=self.offset,
            elapsed_ms=self.started.elapsed().as_millis() as u64,
            capture_wait_ms=self.capture.as_millis() as u64,
            read_verify_ms=self.read.as_millis() as u64,
            network_queue_ms=self.queue.as_millis() as u64,
            ack_wait_ms=self.ack.as_millis() as u64,
            "Sender file stage timings");
    }
}
enum SendPayload {
    Empty,
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    Bytes {
        block: Arc<Vec<u8>>,
        start: usize,
        length: usize,
    },
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Region(usize),
}
type SendPacket = (Message, SendPayload);
// Hold one complete verified block per stream, including its hash/open header.
// A frame-sized queue drains before the next block can be read and verified,
// serializing storage/CPU work with the socket, especially after a resume.
// Per-file acknowledgement windows still bound all in-flight payload bytes.
const SEND_QUEUE: usize = STREAMS * (BLOCK / FRAME + 2);
enum FilePacket {
    Hash { offset: u64, hash: String },
    Data { offset: u64, bytes: Vec<u8> },
    Close,
}
fn reader(mut socket: OwnedReadHalf) -> (Reader, mpsc::Receiver<anyhow::Result<Packet>>) {
    let (tx, rx) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        loop {
            let result = async {
                let message = wire::read(&mut socket).await?;
                let bytes = if matches!(message, Message::Data { .. }) {
                    wire::payload(&mut socket).await?
                } else {
                    vec![]
                };
                Ok((message, bytes))
            }
            .await;
            let failed = result.is_err();
            if tx.send(result).await.is_err() || failed {
                break;
            }
        }
    });
    (Reader(task), rx)
}
async fn next(rx: &mut mpsc::Receiver<anyhow::Result<Packet>>) -> anyhow::Result<Packet> {
    rx.recv()
        .await
        .ok_or_else(|| anyhow::anyhow!("Connection closed; task can be resumed"))?
}

pub async fn send(
    mut task: SavedTask,
    progress: watch::Sender<TransferProgressMsg>,
    id: u64,
    mut control: watch::Receiver<Command>,
) -> anyhow::Result<std::net::SocketAddr> {
    let mut status = TransferProgressMsg {
        id,
        current: 0,
        total: task.manifest.total(),
        status: TransferStatus::Connecting,
        detail: "正在核验接收端检查点…".into(),
    };
    progress.send_replace(status.clone());
    let captures = Arc::new(tokio::sync::RwLock::new(()));
    let result = send_inner(&task, &progress, &mut status, &mut control, &captures).await;
    // Aborting an async worker does not abort its blocking capture. Drain owned
    // capture guards before cleanup or allowing the same task to resume.
    let _captures_drained = captures.write().await;
    status.status = match &result {
        Ok((_, Some(saved))) => {
            task.result = Some(saved.clone());
            status.current = status.total;
            status.detail = format!("保存位置：{saved}");
            TransferStatus::Complete
        }
        Ok((_, None)) => {
            if *control.borrow() == Command::Cancel {
                TransferStatus::Cancelled
            } else {
                TransferStatus::Paused
            }
        }
        Err(error) => TransferStatus::Failed(format!("{error:#}；任务已保留，可继续")),
    };
    let cancelled = matches!(&result, Ok((_, None))) && *control.borrow() == Command::Cancel;
    let persistence = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        if task.result.is_some() {
            storage::save(
                &storage::outgoing(&task.manifest.id).join("task.json"),
                &task,
            )?;
            let data = storage::outgoing(&task.manifest.id).join("data");
            if data.exists() {
                let _ = std::fs::remove_dir_all(data);
            }
        }
        if cancelled {
            storage::discard(&task.manifest.id)?;
        }
        Ok(())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    if let Err(error) = persistence {
        status.status = TransferStatus::Failed(format!(
            "保存本地任务状态失败：{error:#}；可重新继续核验接收结果"
        ));
        progress.send_replace(status);
        return Err(error);
    }
    progress.send_replace(status);
    result.map(|(address, _)| address)
}
async fn send_inner(
    task: &SavedTask,
    progress: &watch::Sender<TransferProgressMsg>,
    status: &mut TransferProgressMsg,
    control: &mut watch::Receiver<Command>,
    captures: &Arc<tokio::sync::RwLock<()>>,
) -> anyhow::Result<(std::net::SocketAddr, Option<String>)> {
    let (mut stream, address) = super::super::connect::connect(task.addresses.clone()).await?;
    stream.set_nodelay(true)?;
    stream.write_all(super::MAGIC).await?;
    wire::json(&mut stream, &Message::Hello { version: 2 }).await?;
    let window = match wire::read(&mut stream).await? {
        Message::Ready {
            streams,
            frame,
            block,
            window,
            streaming,
        } if streams == STREAMS
            && frame == FRAME
            && block == BLOCK
            && (BLOCK..=wire::WINDOW).contains(&window)
            && window.is_multiple_of(BLOCK) =>
        {
            anyhow::ensure!(
                !task.manifest.streaming || streaming,
                "接收端不支持边准备边发送，请更新接收端版本"
            );
            window
        }
        Message::Error { detail, .. } => anyhow::bail!(detail),
        _ => anyhow::bail!("Unsupported transfer protocol"),
    };
    wire::json(
        &mut stream,
        &Message::Begin {
            manifest: task.manifest.clone(),
            token: task.token.clone(),
        },
    )
    .await?;
    let mut offsets = loop {
        match wire::read(&mut stream).await? {
            Message::Resume {
                result: Some(saved),
                ..
            } => return Ok((address, Some(saved))),
            Message::Resume {
                offsets,
                result: None,
            } => break offsets,
            Message::Saving => {}
            Message::Error { detail, .. } => anyhow::bail!(detail),
            _ => anyhow::bail!("Invalid resume response"),
        }
    };
    anyhow::ensure!(
        offsets.len() == task.manifest.entries.len(),
        "Invalid resume map"
    );
    for (offset, entry) in offsets.iter().zip(&task.manifest.entries) {
        anyhow::ensure!(
            *offset <= entry.size && (*offset == entry.size || offset.is_multiple_of(BLOCK as u64)),
            "Invalid checkpoint"
        );
    }
    #[cfg(windows)]
    let mut native = match crate::windows_io::Transmitter::new(&stream) {
        Ok(native) => native,
        Err(error) => {
            bevy::log::debug!(%error,"Windows native socket setup unavailable; using buffered frames");
            None
        }
    };
    #[cfg(windows)]
    let mut native_files = HashMap::<usize, Arc<std::fs::File>>::new();
    let (read, mut write) = stream.into_split();
    let (_reader, mut incoming) = reader(read);
    let (packets, mut outgoing) = mpsc::channel::<SendPacket>(SEND_QUEUE);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let mut payload_files = HashMap::<usize, tokio::fs::File>::new();
    let mut workers = JoinSet::<anyhow::Result<usize>>::new();
    let mut acknowledgements = HashMap::<usize, mpsc::Sender<Message>>::new();
    let mut waiting: std::collections::VecDeque<_> = task
        .manifest
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.directory)
        .map(|(i, _)| i)
        .collect();
    let mut completed = 0;
    let file_count = waiting.len();
    status.current = offsets.iter().sum();
    status.status = TransferStatus::Sending;
    if task.manifest.streaming {
        status.detail = "按块暂存并发送；完成前请保留原文件且勿修改。".into();
    }
    let network_started = std::time::Instant::now();
    progress.send_replace(status.clone());
    let mut reported = std::time::Instant::now();
    let mut commands_open = control.has_changed().is_ok();
    loop {
        if reported.elapsed() >= std::time::Duration::from_millis(100) {
            status.detail = format!(
                "文件 {completed}/{file_count} · 活跃流 {}\n{}{}",
                workers.len(),
                acknowledgements
                    .keys()
                    .map(|&i| format!(
                        "{}：{} / {} 字节",
                        task.manifest.entries[i].path.join("/"),
                        offsets[i],
                        task.manifest.entries[i].size
                    ))
                    .collect::<Vec<_>>()
                    .join("\n"),
                if task.manifest.streaming {
                    "\n按块暂存并发送；完成前请保留原文件且勿修改。"
                } else {
                    ""
                }
            );
            progress.send_replace(status.clone());
            reported = std::time::Instant::now();
        }
        while workers.len() < STREAMS && *control.borrow() == Command::Run {
            let Some(file) = waiting.pop_front() else {
                break;
            };
            let (ack_tx, ack_rx) = mpsc::channel(window / BLOCK + 1);
            acknowledgements.insert(file, ack_tx);
            let packets = packets.clone();
            let entry = task.manifest.entries[file].clone();
            let path = storage::outgoing(&task.manifest.id)
                .join("data")
                .join(file.to_string());
            let offset = offsets[file];
            workers.spawn(send_file(
                file,
                path,
                entry,
                offset,
                window,
                task.manifest.streaming,
                captures.clone(),
                packets,
                ack_rx,
            ));
        }
        if *control.borrow() != Command::Run {
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            let cancel = *control.borrow() == Command::Cancel;
            wire::json(
                &mut write,
                &if cancel {
                    Message::Cancel
                } else {
                    Message::Pause
                },
            )
            .await?;
            loop {
                match next(&mut incoming).await?.0 {
                    Message::Stopped => return Ok((address, None)),
                    Message::Error { detail, .. } => anyhow::bail!(detail),
                    _ => {}
                }
            }
        }
        if completed == file_count {
            break;
        }
        tokio::select! {
            changed = control.changed(), if commands_open => { commands_open = changed.is_ok(); },
            packet = outgoing.recv() => {
                let (message,data) = packet.ok_or_else(||anyhow::anyhow!("File queue closed"))?;
                wire::json(&mut write,&message).await?;
                match data {
                    SendPayload::Empty => {},
                    #[cfg(not(any(target_os="linux",target_os="android",windows)))]
                    SendPayload::Bytes {block,start,length} => wire::data(&mut write,&block[start..start+length]).await?,
                    #[cfg(windows)]
                    SendPayload::Bytes {block,start,length} => {
                        let Message::Data {file,offset}=message else {anyhow::bail!("Invalid native frame");};
                        if let Some(transmitter)=&native {
                            if let std::collections::hash_map::Entry::Vacant(entry)=native_files.entry(file) {
                                let path=storage::outgoing(&task.manifest.id).join("data").join(file.to_string());
                                let handle=tokio::task::spawn_blocking(move|| {
                                    use std::os::windows::fs::OpenOptionsExt;
                                    std::fs::File::options().read(true)
                                        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_SEQUENTIAL_SCAN)
                                        .open(path)
                                }).await??;
                                entry.insert(Arc::new(handle));
                            }
                            tokio::time::timeout(wire::IDLE,write.write_u32_le(length as u32)).await??;
                            let sent=transmitter.send(native_files[&file].clone(),offset,length,wire::IDLE).await?;
                            if !sent {
                                // No operation was initiated. The length prefix
                                // is already sent: append payload, not a new frame.
                                native=None;native_files.clear();
                                tokio::time::timeout(wire::IDLE,write.write_all(&block[start..start+length])).await??;
                                bevy::log::debug!("TransmitFile unsupported; buffered fallback for this connection");
                            }
                        } else {
                            wire::data(&mut write,&block[start..start+length]).await?;
                        }
                    }
                    #[cfg(any(target_os="linux",target_os="android"))]
                    SendPayload::Region(length) => {
                        let Message::Data {file,offset}=message else {anyhow::bail!("Invalid payload region");};
                        if let std::collections::hash_map::Entry::Vacant(entry)=payload_files.entry(file) {
                            entry.insert(tokio::fs::File::open(storage::outgoing(&task.manifest.id).join("data").join(file.to_string())).await?);
                        }
                        let input=payload_files.get_mut(&file).unwrap();
                        input.seek(std::io::SeekFrom::Start(offset)).await?;
                        tokio::time::timeout(wire::IDLE, write.write_u32_le(length as u32)).await??;
                        // Only this writer touches the socket; each complete frame
                        // finishes before another stream's header is scheduled.
                        crate::kernel_copy::copy(write.as_ref(),input,length as u64,crate::kernel_copy::Direction::Send, |_|{}).await?;
                    }
                }
                #[cfg(any(target_os="linux",target_os="android"))]
                if let Message::Close {file}=message {payload_files.remove(&file);}
                #[cfg(windows)]
                if let Message::Close {file}=message {native_files.remove(&file);}
            }
            packet = next(&mut incoming) => {
                let message = packet?.0;
                match message {
                    Message::Checkpoint {file,offset,credit,durable} => {
                        anyhow::ensure!(file < offsets.len() && offset >= offsets[file] && offset <= task.manifest.entries[file].size && credit <= BLOCK, "Invalid checkpoint");
                        anyhow::ensure!(durable<=offset,"Invalid durable checkpoint");
                        offsets[file] = offset;
                        status.current = offsets.iter().sum(); progress.send_replace(status.clone());
                        acknowledgements.get(&file).ok_or_else(||anyhow::anyhow!("Unknown stream"))?.try_send(Message::Checkpoint {file,offset,credit,durable})?;
                    }
                    Message::Closed {file} => {
                        acknowledgements.get(&file).ok_or_else(||anyhow::anyhow!("Unknown stream"))?.try_send(Message::Closed {file})?;
                    }
                    Message::Error {detail,..} => anyhow::bail!(detail),
                    _ => anyhow::bail!("Unexpected control message"),
                }
            }
            result = workers.join_next(), if !workers.is_empty() => {
                let file = result.unwrap()??;
                acknowledgements.remove(&file); completed += 1;
            }
        }
    }
    status.status = TransferStatus::AwaitingConfirmation;
    bevy::log::info!(task=%task.manifest.id,elapsed_ms=network_started.elapsed().as_millis() as u64,"Network transfer phase finished");
    progress.send_replace(status.clone());
    wire::json(&mut write, &Message::Commit).await?;
    loop {
        match next(&mut incoming).await?.0 {
            Message::Complete { result } => return Ok((address, Some(result))),
            Message::Saving => {
                status.detail = "接收端正在写入并保存文件，请稍候；保存耗时取决于目标存储。".into();
                progress.send_replace(status.clone());
            }
            Message::SaveProgress { current, total } => {
                status.detail = format!(
                    "接收端保存进度：{:.1} / {:.1} MiB",
                    current as f64 / 1048576.0,
                    total as f64 / 1048576.0
                );
                progress.send_replace(status.clone());
            }
            Message::Error { detail, .. } => anyhow::bail!(detail),
            _ => anyhow::bail!("Invalid final confirmation"),
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn send_file(
    file: usize,
    path: std::path::PathBuf,
    entry: super::Entry,
    mut offset: u64,
    window: usize,
    streaming: bool,
    captures: Arc<tokio::sync::RwLock<()>>,
    packets: mpsc::Sender<SendPacket>,
    mut ack: mpsc::Receiver<Message>,
) -> anyhow::Result<usize> {
    let mut timings = SendTimings {
        path: path.clone(),
        offset,
        started: std::time::Instant::now(),
        capture: Default::default(),
        read: Default::default(),
        queue: Default::default(),
        ack: Default::default(),
    };
    let mut input = tokio::fs::File::open(&path).await?;
    // The receiver has revalidated the accepted prefix against the manifest.
    // Hash only blocks actually resent; rereading a multi-GiB accepted prefix
    // delays all streams without adding integrity to the received content.
    let mut buffer = vec![0; BLOCK];
    input.seek(std::io::SeekFrom::Start(offset)).await?;
    packets
        .send((Message::Open { file }, SendPayload::Empty))
        .await?;
    let mut pending = std::collections::VecDeque::new();
    let mut ahead = if streaming {
        Some(CaptureAhead::start(path.clone(), entry.size, offset, captures.clone()).await)
    } else {
        None
    };
    while offset < entry.size || !pending.is_empty() {
        while offset < entry.size && pending.len() < window / BLOCK {
            let n = (entry.size - offset).min(BLOCK as u64) as usize;
            if streaming {
                let started = std::time::Instant::now();
                ahead.as_mut().unwrap().wait(offset + n as u64).await?;
                timings.capture += started.elapsed();
                let started = std::time::Instant::now();
                let path = path.clone();
                let size = entry.size;
                let guard = captures.clone().read_owned().await;
                let (bytes, hash) = tokio::task::spawn_blocking(move || {
                    let _guard = guard;
                    super::preparation::read_captured(&path, size, offset)
                })
                .await??;
                buffer = bytes;
                timings.read += started.elapsed();
                packets
                    .send((Message::Hash { file, offset, hash }, SendPayload::Empty))
                    .await?;
            } else {
                input.read_exact(&mut buffer[..n]).await?;
                // Hashing a multi-MiB block is CPU work; keep it off the async socket
                // executor while the previously queued block is being transmitted.
                let expected = entry.blocks[(offset / BLOCK as u64) as usize].clone();
                buffer = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
                    anyhow::ensure!(
                        blake3::hash(&buffer[..n]).to_hex().as_str() == expected,
                        "Source snapshot corrupted"
                    );
                    Ok(buffer)
                })
                .await??;
            }
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let shared = Arc::new(std::mem::take(&mut buffer));
            let queued = std::time::Instant::now();
            for start in (0..n).step_by(FRAME) {
                let length = (n - start).min(FRAME);
                packets
                    .send((
                        Message::Data {
                            file,
                            offset: offset + start as u64,
                        },
                        {
                            #[cfg(any(target_os = "linux", target_os = "android"))]
                            {
                                SendPayload::Region(length)
                            }
                            #[cfg(not(any(target_os = "linux", target_os = "android")))]
                            {
                                SendPayload::Bytes {
                                    block: shared.clone(),
                                    start,
                                    length,
                                }
                            }
                        },
                    ))
                    .await?;
            }
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            {
                // Reuse the verified block once the socket writer has released all
                // frame references; otherwise allocate at most one next block.
                buffer = Arc::try_unwrap(shared).unwrap_or_else(|_| vec![0; BLOCK]);
            }
            offset += n as u64;
            timings.queue += queued.elapsed();
            if let Some(ahead) = &ahead {
                ahead.consumed.send_replace(offset);
            }
            pending.push_back((offset, n));
        }
        let (expected, length) = pending
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("Missing in-flight block"))?;
        let started = std::time::Instant::now();
        let checkpoint = ack.recv().await;
        timings.ack += started.elapsed();
        match checkpoint {
            Some(Message::Checkpoint {
                offset: accepted,
                credit,
                ..
            }) if accepted == expected && credit == length => {}
            _ => anyhow::bail!("Checkpoint missing"),
        }
    }
    packets
        .send((Message::Close { file }, SendPayload::Empty))
        .await?;
    anyhow::ensure!(
        matches!(ack.recv().await, Some(Message::Closed { .. })),
        "File completion missing"
    );
    Ok(file)
}

pub(crate) async fn receive(
    mut stream: TcpStream,
    destination: portalo_core::settings::ReceiveDestination,
    mut progress: super::super::receive_progress::ReceiveProgress,
) -> anyhow::Result<()> {
    let result = receive_inner(&mut stream, destination, &mut progress).await;
    if let Err(error) = &result {
        let _ = wire::json(
            &mut stream,
            &Message::Error {
                code: "TRANSFER_FAILED".into(),
                detail: format!("{error:#}"),
            },
        )
        .await;
    }
    progress.finish(result.as_ref().err().map(|e| format!("{e:#}")));
    result
}

async fn receive_inner(
    stream: &mut TcpStream,
    destination: portalo_core::settings::ReceiveDestination,
    progress: &mut super::super::receive_progress::ReceiveProgress,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(wire::read(stream).await?, Message::Hello { version: 2 }),
        "Unsupported protocol version"
    );
    wire::json(
        stream,
        &Message::Ready {
            streams: STREAMS,
            frame: FRAME,
            block: BLOCK,
            window: wire::WINDOW,
            streaming: true,
        },
    )
    .await?;
    let Message::Begin { manifest, token } = wire::read(stream).await? else {
        anyhow::bail!("Missing manifest");
    };
    progress.header(&manifest.name, manifest.total());
    let session = finish_commit(
        stream,
        tokio::task::spawn_blocking(move || Session::open(manifest, token, destination)),
        || Message::Saving,
    )
    .await?;
    let receipt = session.receipt.lock().unwrap().clone();
    wire::json(
        stream,
        &Message::Resume {
            offsets: receipt.offsets.clone(),
            result: receipt.result.clone(),
        },
    )
    .await?;
    if receipt.result.is_some() {
        progress.receiving();
        progress.advance(session.manifest.total());
        progress.saving();
        progress.saved();
        return Ok(());
    }
    progress.receiving();
    progress.advance(receipt.offsets.iter().sum());
    // Borrowed split lets the outer handler retain the socket to send ERROR.
    let (mut read, mut write) = stream.split();
    let (input_tx, mut input_rx) = mpsc::channel::<anyhow::Result<Packet>>(1);
    let read_loop = async {
        loop {
            let packet = async {
                let message: Message = wire::read(&mut read).await?;
                let data = if matches!(message, Message::Data { .. }) {
                    wire::payload(&mut read).await?
                } else {
                    vec![]
                };
                Ok((message, data))
            }
            .await;
            let failed = packet.is_err();
            if input_tx.send(packet).await.is_err() || failed {
                break;
            }
        }
        // Let the protocol processor consume errors in order. In particular,
        // EOF or an idle read after COMMIT must not drop an ongoing publication.
        std::future::pending::<anyhow::Result<()>>().await
    };
    let process = async {
        let (events, mut results) = mpsc::channel::<Message>(STREAMS);
        let mut active: HashMap<usize, mpsc::Sender<FilePacket>> = HashMap::new();
        let mut workers = JoinSet::new();
        let mut closed = HashSet::new();
        let mut credits = HashMap::<usize, usize>::new();
        let mut offsets = receipt.offsets;
        let mut sync_jobs = JoinSet::new();
        let mut sync_tick = tokio::time::interval(std::time::Duration::from_secs(1));
        sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = sync_tick.tick(), if sync_jobs.is_empty() => {
                    let s=session.clone();
                    sync_jobs.spawn_blocking(move||s.flush());
                }
                _ = session.sync_requested.notified(), if sync_jobs.is_empty() => {
                    let s=session.clone();
                    sync_jobs.spawn_blocking(move||s.flush());
                }
                result=sync_jobs.join_next(), if !sync_jobs.is_empty() => {
                    result.unwrap()??;
                }
                message = results.recv() => {
                    if let Some(Message::Checkpoint {file,offset,credit,durable}) = message {
                        progress.advance(offset-offsets[file]); offsets[file]=offset;
                        *credits.get_mut(&file).ok_or_else(||anyhow::anyhow!("Unknown credit stream"))? += credit;
                        wire::json(&mut write,&Message::Checkpoint {file,offset,credit,durable}).await?;
                    }
                }
                result = workers.join_next(), if !workers.is_empty() => {
                    let file = result.unwrap()??;
                    active.remove(&file); credits.remove(&file); closed.insert(file);
                    wire::json(&mut write,&Message::Closed {file}).await?;
                }
                packet = next(&mut input_rx) => {
                    let (message,data) = packet?;
                    match message {
                        Message::Open {file} => {
                            anyhow::ensure!(file < session.manifest.entries.len() && !session.manifest.entries[file].directory && !active.contains_key(&file) && !closed.contains(&file) && active.len()<STREAMS,"Invalid stream open");
                            let (tx,rx)=mpsc::channel(wire::WINDOW/FRAME+wire::WINDOW/BLOCK+1);
                            active.insert(file,tx);
                            credits.insert(file,wire::WINDOW);
                            workers.spawn(receive_file(session.clone(),file,rx,events.clone()));
                        }
                        Message::Data {file,offset} => {
                            let tx=active.get(&file).ok_or_else(||anyhow::anyhow!("Unknown file stream"))?;
                            let credit=credits.get_mut(&file).unwrap();
                            anyhow::ensure!(data.len()<=*credit,"Stream window exceeded");
                            *credit-=data.len();
                            tx.try_send(FilePacket::Data {offset,bytes:data}).map_err(|_|anyhow::anyhow!("Flow control exceeded"))?;
                        }
                        Message::Hash {file,offset,hash} => {
                            anyhow::ensure!(session.manifest.streaming && hash.len()==64 && hash.bytes().all(|byte|byte.is_ascii_hexdigit()),"Invalid streaming hash");
                            active.get(&file).ok_or_else(||anyhow::anyhow!("Unknown hash stream"))?
                                .try_send(FilePacket::Hash {offset,hash}).map_err(|_|anyhow::anyhow!("Hash flow control exceeded"))?;
                        }
                        Message::Close {file} => {
                            active.get(&file).ok_or_else(||anyhow::anyhow!("Unknown file stream"))?.send(FilePacket::Close).await?;
                        }
                        Message::Pause | Message::Cancel => {
                            let cancel=matches!(message,Message::Cancel);
                            active.clear();
                            // Drain rather than abort: outstanding fsync/checkpoint
                            // jobs must finish before deleting a cancelled task.
                            while !workers.is_empty() {
                                tokio::select! {
                                    _ = workers.join_next() => {},
                                    _ = results.recv() => {},
                                }
                            }
                            while let Some(result)=sync_jobs.join_next().await {result??;}
                            let s=session.clone();
                            tokio::task::spawn_blocking(move||if cancel {s.cancel()} else {s.flush()}).await??;
                            wire::json(&mut write,&Message::Stopped).await?;
                            anyhow::bail!(if cancel {"任务已取消"} else {"任务已暂停，可继续"});
                        }
                        Message::Commit => {
                            anyhow::ensure!(active.is_empty() && closed.len()==session.manifest.entries.iter().filter(|e|!e.directory).count(),"Streams still active");
                            while let Some(result)=sync_jobs.join_next().await {result??;}
                            progress.saving();
                            #[cfg(target_os="android")]
                            let staged = {
                                let receipt=session.receipt.lock().unwrap();
                                receipt.document_tree.is_some() && receipt.direct.is_none()
                            };
                            let s=session.clone();
                            let result=finish_commit(&mut write, tokio::task::spawn_blocking(move||s.commit()), || {
                                #[cfg(target_os="android")]
                                if staged && let Ok(current)=portalo_core::android_storage::transfer_save_progress(&session.manifest.id) {
                                    let current=current.min(session.manifest.total());
                                    progress.saving_bytes(current);
                                    return Message::SaveProgress {current,total:session.manifest.total()};
                                }
                                Message::Saving
                            }).await?;
                            progress.saved();
                            wire::json(&mut write,&Message::Complete {result}).await?;
                            return Ok::<(),anyhow::Error>(());
                        }
                        _ => anyhow::bail!("Unexpected frame"),
                    }
                }
            }
        }
    };
    tokio::pin!(read_loop, process);
    tokio::select! { result=&mut read_loop=>result, result=&mut process=>result }
}

/// Keep publication owned until it finishes, even if the sender disconnects.
/// The session lock stays held and reconnects cannot start another publication.
async fn finish_commit<W: tokio::io::AsyncWrite + Unpin, T>(
    write: &mut W,
    mut job: tokio::task::JoinHandle<anyhow::Result<T>>,
    mut progress: impl FnMut() -> Message,
) -> anyhow::Result<T> {
    let interval = std::time::Duration::from_secs(10);
    let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut connected = true;
    loop {
        tokio::select! {
            result = &mut job => return result?,
            _ = heartbeat.tick() => {
                // This is a liveness notification, not a fabricated byte count.
                let update = progress();
                if connected { connected = wire::json(write, &update).await.is_ok(); }
            }
        }
    }
}

async fn receive_file(
    session: Arc<Session>,
    file: usize,
    mut rx: mpsc::Receiver<FilePacket>,
    events: mpsc::Sender<Message>,
) -> anyhow::Result<usize> {
    let mut offset = session.receipt.lock().unwrap().offsets[file];
    let size = session.manifest.entries[file].size;
    let s = session.clone();
    let payload = tokio::task::spawn_blocking(move || s.open_payload(file)).await??;
    let mut buffer = Vec::new();
    let mut hash = None;
    while let Some(packet) = rx.recv().await {
        let (position, data) = match packet {
            FilePacket::Hash {
                offset: position,
                hash: digest,
            } => {
                anyhow::ensure!(
                    session.manifest.streaming
                        && position == offset
                        && hash.is_none()
                        && buffer.is_empty()
                        && offset < size,
                    "Out of order streaming hash"
                );
                hash = Some(digest);
                continue;
            }
            FilePacket::Data { offset, bytes } => (offset, bytes),
            FilePacket::Close => {
                anyhow::ensure!(
                    buffer.is_empty() && offset == size && hash.is_none(),
                    "Incomplete file stream"
                );
                let s = session.clone();
                tokio::task::spawn_blocking(move || s.flush()).await??;
                return Ok(file);
            }
        };
        anyhow::ensure!(
            !session.manifest.streaming || hash.is_some(),
            "Missing streaming block hash"
        );
        anyhow::ensure!(
            position == offset + buffer.len() as u64
                && data.len() <= FRAME
                && position + data.len() as u64 <= size
                && buffer.len() + data.len() <= BLOCK,
            "Invalid data position/window"
        );
        buffer.extend_from_slice(&data);
        if buffer.len() == BLOCK || offset + buffer.len() as u64 == size {
            let credit = buffer.len();
            let s = session.clone();
            let payload = payload.clone();
            let bytes = std::mem::take(&mut buffer);
            let expected = hash.take();
            offset = tokio::task::spawn_blocking(move || match expected {
                Some(hash) => s.write_stream_block(&payload, offset, &bytes, &hash),
                None => s.write_block(&payload, offset, &bytes),
            })
            .await??;
            let durable = session.receipt.lock().unwrap().offsets[file];
            events
                .send(Message::Checkpoint {
                    file,
                    offset,
                    credit,
                    durable,
                })
                .await?;
        }
    }
    anyhow::bail!("Stream interrupted; checkpoint retained")
}

#[cfg(test)]
mod commit_tests {
    use super::*;

    #[test]
    fn old_ready_message_uses_one_block_window() {
        let value = serde_json::json!({"Ready":{"streams":STREAMS,"frame":FRAME,"block":BLOCK}});
        let Message::Ready { window, .. } = serde_json::from_value(value).unwrap() else {
            panic!("Expected Ready");
        };
        assert_eq!(window, BLOCK);
    }

    #[tokio::test]
    async fn capture_runs_ahead_of_network_window_and_drains_on_drop() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("source");
        let size = 80 * 1024 * 1024;
        std::fs::File::create(&path).unwrap().set_len(size).unwrap();
        let (_tx, control) = watch::channel(Command::Run);
        let task = super::super::prepare_streaming_with_progress(
            vec![crate::SendSource::Path(path)],
            vec![],
            &control,
            &|_, _| {},
        )
        .unwrap();
        let snapshot = storage::outgoing(&task.manifest.id).join("data/0");
        let captures = Arc::new(tokio::sync::RwLock::new(()));
        let mut ahead = CaptureAhead::start(snapshot, size, 0, captures.clone()).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            ahead.wait(32 * 1024 * 1024),
        )
        .await
        .unwrap()
        .unwrap();
        let ready = ahead.ready.borrow().clone().unwrap();
        assert!(ready > wire::WINDOW as u64 && ready <= 64 * 1024 * 1024);
        drop(ahead);
        let _drained = tokio::time::timeout(std::time::Duration::from_secs(10), captures.write())
            .await
            .unwrap();
        storage::discard(&task.manifest.id).unwrap();
    }

    #[tokio::test]
    async fn sender_verifies_next_block_while_previous_block_is_queued() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("snapshot");
        let bytes = vec![31; BLOCK + 1];
        std::fs::write(&path, &bytes).unwrap();
        let entry = super::super::Entry {
            path: vec!["snapshot".into()],
            directory: false,
            size: bytes.len() as u64,
            blocks: vec![
                blake3::hash(&bytes[..BLOCK]).to_hex().to_string(),
                "wrong".into(),
            ],
        };
        let (packets, _receive) = mpsc::channel(SEND_QUEUE);
        let (_ack, acks) = mpsc::channel(3);
        // Do not consume any network frames. The second block must already be
        // checked while the first block is still waiting for the socket writer.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            send_file(
                0,
                path,
                entry,
                0,
                wire::WINDOW,
                false,
                Arc::new(tokio::sync::RwLock::new(())),
                packets,
                acks,
            ),
        )
        .await
        .unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Source snapshot corrupted")
        );
    }

    #[tokio::test]
    async fn sender_pipelines_two_blocks_but_closes_only_after_all_acks() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("snapshot");
        let bytes = vec![31; 2 * BLOCK + 17];
        std::fs::write(&path, &bytes).unwrap();
        let entry = super::super::Entry {
            path: vec!["snapshot".into()],
            directory: false,
            size: bytes.len() as u64,
            blocks: bytes
                .chunks(BLOCK)
                .map(|block| blake3::hash(block).to_hex().to_string())
                .collect(),
        };
        let (packets, mut receive) = mpsc::channel(4);
        let (ack, acks) = mpsc::channel(3);
        let sender = tokio::spawn(send_file(
            0,
            path,
            entry,
            0,
            wire::WINDOW,
            false,
            Arc::new(tokio::sync::RwLock::new(())),
            packets,
            acks,
        ));
        let timeout = std::time::Duration::from_secs(10);
        assert!(matches!(
            tokio::time::timeout(timeout, receive.recv())
                .await
                .unwrap()
                .unwrap()
                .0,
            Message::Open { file: 0 }
        ));
        let mut offset = 0;
        while offset < 2 * BLOCK {
            let (message, payload) = tokio::time::timeout(timeout, receive.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(message,Message::Data {file:0,offset:position} if position==offset as u64)
            );
            let length = match payload {
                #[cfg(any(target_os = "linux", target_os = "android"))]
                SendPayload::Region(length) => length,
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                SendPayload::Bytes { length, .. } => length,
                _ => panic!("Missing payload"),
            };
            offset += length;
        }
        // No ACK has been sent: both blocks must have reached the bounded queue.
        ack.send(Message::Checkpoint {
            file: 0,
            offset: BLOCK as u64,
            credit: BLOCK,
            durable: 0,
        })
        .await
        .unwrap();
        let (message, _) = tokio::time::timeout(timeout, receive.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(message,Message::Data {file:0,offset} if offset==(2*BLOCK) as u64));
        ack.send(Message::Checkpoint {
            file: 0,
            offset: (2 * BLOCK) as u64,
            credit: BLOCK,
            durable: 0,
        })
        .await
        .unwrap();
        ack.send(Message::Checkpoint {
            file: 0,
            offset: bytes.len() as u64,
            credit: 17,
            durable: 0,
        })
        .await
        .unwrap();
        assert!(matches!(
            tokio::time::timeout(timeout, receive.recv())
                .await
                .unwrap()
                .unwrap()
                .0,
            Message::Close { file: 0 }
        ));
        ack.send(Message::Closed { file: 0 }).await.unwrap();
        assert_eq!(
            tokio::time::timeout(timeout, sender)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn slow_publication_keeps_peer_alive_past_idle_deadline() {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        let job = tokio::spawn(async {
            tokio::time::sleep(wire::IDLE * 3).await;
            Ok("saved".to_owned())
        });
        let server = tokio::spawn(async move {
            let result = finish_commit(&mut writer, job, || Message::Saving)
                .await
                .unwrap();
            wire::json(&mut writer, &Message::Complete { result })
                .await
                .unwrap();
        });
        let start = tokio::time::Instant::now();
        let mut notifications = 0;
        loop {
            match wire::read(&mut reader).await.unwrap() {
                Message::Saving => notifications += 1,
                Message::Complete { result } => {
                    assert_eq!(result, "saved");
                    break;
                }
                other => panic!("Unexpected {other:?}"),
            }
        }
        assert!(start.elapsed() >= wire::IDLE * 3);
        assert!(notifications >= 30);
        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn disconnected_sender_does_not_abandon_publication() {
        let (mut writer, reader) = tokio::io::duplex(64);
        drop(reader);
        let job = tokio::spawn(async {
            tokio::time::sleep(wire::IDLE * 2).await;
            Ok("durable completion".to_owned())
        });
        assert_eq!(
            finish_commit(&mut writer, job, || Message::Saving)
                .await
                .unwrap(),
            "durable completion"
        );
    }
}
