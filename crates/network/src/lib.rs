use bevy::prelude::*;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufWriter, ReadBuf};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use portalo_core::{FileTransferState, ProgressReceiver, ProgressSender, TransferProgressMsg};

// 网络插件
// --------------- PLUGIN --------------- //
pub struct NetworkPlugin;

impl Plugin for NetworkPlugin {
    fn build(&self, app: &mut App) {
        // 创建一个并行的 Tokio Runtime 并作为 Resource 存入 Bevy
        // 这样插件内部和用户都能访问到同一个 Runtime
        // 使用 Builder 模式可以更好地控制线程数（移动端优化）
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");

        let (tx, rx) = mpsc::unbounded_channel();

        app.insert_resource(TokioRuntime(runtime))
            .insert_resource(ProgressSender(tx))
            .insert_resource(ProgressReceiver(rx))
            .add_systems(
                Startup,
                (setup_network_listener, update_transfer_state_system),
            );
    }
}

// --------------- SETUP --------------- //
fn setup_network_listener(runtime: Res<TokioRuntime>) {
    // 拿到 handle，它是轻量级可克隆的
    let handle = runtime.0.handle().clone();
    // TODO: 放到配置
    let save_path = PathBuf::from("./downloads");

    // 使用 tokio 的 spawn，它会自动关联 Reactor
    handle.clone().spawn(async move {
        // 确保下载目录存在
        let _ = tokio::fs::create_dir_all(&save_path).await;

        let listener = match tokio::net::TcpListener::bind("0.0.0.0:49527").await {
            Ok(l) => l,
            Err(e) => {
                error!("# 无法绑定端口 49527: {}", e);
                return;
            }
        };
        info!("# 插件监听已启动: 49527");

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("# 收到来自 {} 的连接请求", addr);
                    let path = save_path.clone();

                    // 关键：必须使用 handle.spawn 来启动每一个文件接收任务
                    // 这样即使主监听循环在忙，接收任务也能在线程池中并行处理
                    handle.spawn(async move {
                        if let Err(e) = start_file_receiver(stream, path).await {
                            error!("# 接收文件时出错 (来自 {}): {:?}", addr, e);
                        }
                    });
                }
                Err(e) => error!("# TCP Accept Error: {}", e),
            }
        }
    });
}

// --------------- RESOURCES --------------- //
#[derive(Resource)]
pub struct TokioRuntime(pub Runtime);

// --------------- COMPONENTS --------------- //

// --------------- SYSTEMS --------------- //
// 接收文件逻辑
pub async fn start_file_receiver(
    mut stream: tokio::net::TcpStream,
    save_path: PathBuf,
) -> anyhow::Result<()> {
    // 设置缓冲区优化 (针对移动端/桌面端平衡)

    // 解析协议头: |文件名长度|文件名称|文件大小|
    // 文件名长度
    let name_len = stream.read_u32_le().await? as usize;
    // 防止文件名长度异常导致内存溢出
    if name_len > 4096 {
        return Err(anyhow::anyhow!("File name too long: {}", name_len));
    }

    // 文件名
    let mut name_buf = vec![0u8; name_len];
    stream.read_exact(&mut name_buf).await?;
    let file_name = String::from_utf8_lossy(&name_buf).to_string();
    // 过滤文件名，防止路径穿越攻击
    let safe_file_name = std::path::Path::new(&file_name)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid file name"))?;

    // 文件大小
    let file_size = stream.read_u64_le().await?;

    // TODO: 使用设置的文件夹
    // 创建文件夹及文件，并打开句柄
    tokio::fs::create_dir_all(&save_path).await?;
    let full_path = save_path.join(safe_file_name);
    let file = File::create(&full_path).await?;
    let mut writer = BufWriter::with_capacity(128 * 1024, file);

    // 最后活跃时间
    let last_active = Arc::new(AtomicU64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    ));
    // 零拷贝写入
    let mut progress_reader = ProgressReader {
        inner: stream.take(file_size),
        total: file_size,
        current: 0,
        last_reported: 0,
        last_active: last_active.clone(),
        on_progress: Box::new(|c, t| {
            // TODO: 进度回调
        }),
    };

    // 使用 select 监听两个 Future
    tokio::select! {
        // 任务 A: 正常拷贝数据
        res = tokio::io::copy(&mut progress_reader, &mut writer) => {
            res?;
            writer.flush().await?;
            info!("# 文件接收成功: {}", file_name);
        }
        // 任务 B: 监控超时
        _ = async {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let last = last_active.load(Ordering::Relaxed);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                if now - last > 30 {
                    break; // 超过 30 秒没更新了，跳出循环
                }
            }
        } => {
            return Err(anyhow::anyhow!("传输超时：连续 30 秒无数据交换"));
        }
    }

    Ok(())
}

// 发送文件逻辑
pub async fn send_file_fast(
    dest_addr: String,
    file_path: impl AsRef<std::path::Path>,
    progress_tx: mpsc::UnboundedSender<TransferProgressMsg>,
) -> anyhow::Result<()> {
    // 建立连接 (Dukto 默认端口 49527)
    let mut stream = tokio::net::TcpStream::connect(&dest_addr).await?;

    // 设置 TCP_NODELAY 减少延迟
    stream.set_nodelay(true)?;
    // TODO: 调整适合小块数据

    // 打开文件并获取元数据
    let file = File::open(&file_path).await?;
    let metadata = file.metadata().await?;
    // 文件大小
    let file_size = metadata.len();
    // 文件名
    let file_path_ref = file_path.as_ref();
    let file_name = file_path_ref
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!("无法从路径中解析有效文件名: {}", file_path_ref.display())
        })?;

    // 协议头: |文件名长度|文件名称|文件大小|
    stream.write_u32_le(file_name.len() as u32).await?;
    stream.write_all(file_name.as_bytes()).await?;
    stream.write_u64_le(file_size).await?;
    // 发送协议头
    stream.flush().await?;

    let default_active = || std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let tx = progress_tx.clone();
    // 监控进度
    let mut progress_reader = ProgressReader {
        inner: file,
        total: file_size,
        current: 0,
        last_reported: 0,
        last_active: default_active(),
        on_progress: Box::new(move |c, t| {
            let _ = tx.send(TransferProgressMsg {
                current: c,
                total: t,
            });
        }),
    };

    // 零拷贝发送
    let bytes_sent = tokio::io::copy(&mut progress_reader, &mut stream).await?;

    info!("# 传输完成: {} ({} bytes)", file_name, bytes_sent);
    Ok(())
}

// 定义一个包装器，记录经过的字节数
struct ProgressReader<R> {
    inner: R,
    total: u64,
    current: u64,
    last_reported: u64,                                         // 最新的进度(字节)
    on_progress: Box<dyn Fn(u64, u64) + Send + Sync + 'static>, // 传入回调或 Channel
    last_active: Arc<AtomicU64>, // 记录最后一次读到数据的时间（秒级戳）
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);

        if let Poll::Ready(Ok(())) = poll {
            let n = buf.filled().len() - before;
            if n > 0 {
                self.current += n as u64;
                // 读到数据了，更新时间戳
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                self.last_active.store(now, Ordering::Relaxed);

                // 频率控制：每增加 1MB 或者传输结束时才回调
                // 1024 * 1024 = 1MB
                if self.current - self.last_reported >= 1024 * 1024 || self.current == self.total {
                    self.last_reported = self.current;
                    (self.on_progress)(self.current, self.total);
                }
            }
        }
        poll
    }
}

fn update_transfer_state_system(
    mut receiver: ResMut<ProgressReceiver>,
    mut state: ResMut<FileTransferState>,
) {
    // 非阻塞地读取所有最新进度
    while let Ok(msg) = receiver.0.try_recv() {
        // 假设 FileTransferState 有这两个字段
        state.bytes_transferred = msg.current;
        state.total_bytes = msg.total;
    }
}
