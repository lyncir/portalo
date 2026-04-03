use std::time::Duration;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::time::timeout;
use bevy::prelude::*;
use futures_util::StreamExt;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter, AsyncRead, ReadBuf};
use tokio::runtime::Runtime;
use tokio_util::io::ReaderStream;

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

        app.insert_resource(TokioRuntime(runtime));
        app.add_systems(Startup, setup_network_listener);
    }
}

// --------------- SETUP --------------- //
fn setup_network_listener(runtime: Res<TokioRuntime>) {
    // 拿到 handle，它是轻量级可克隆的
    let handle = runtime.0.handle().clone();
    let save_path = PathBuf::from("./downloads");

    // 使用 tokio 的 spawn，它会自动关联 Reactor
    handle.clone().spawn(async move {
        // 确保下载目录存在
        let _ = tokio::fs::create_dir_all(&save_path).await;

        let listener = match tokio::net::TcpListener::bind("0.0.0.0:49527").await {
            Ok(l) => l,
            Err(e) => {
                error!("❌ 无法绑定端口 49527: {}", e);
                return;
            }
        };
        info!("👂 插件监听已启动: 49527");

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("📥 收到来自 {} 的连接请求", addr);
                    let path = save_path.clone();

                    // 关键：必须使用 handle.spawn 来启动每一个文件接收任务
                    // 这样即使主监听循环在忙，接收任务也能在线程池中并行处理
                    handle.spawn(async move {
                        if let Err(e) = start_file_receiver(stream, path).await {
                            error!("❌ 接收文件时出错 (来自 {}): {:?}", addr, e);
                        }
                    });
                }
                Err(e) => error!("⚠️ TCP Accept Error: {}", e),
            }
        }
    });
}

// --------------- RESOURCES --------------- //
#[derive(Resource)]
pub struct TokioRuntime(pub Runtime);

// --------------- COMPONENTS --------------- //

// --------------- SYSTEMS --------------- //
// 接收任务的逻辑
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

    println!("{:?}", safe_file_name);
    println!("{}", file_size);

    // 创建文件夹及文件，并打开句柄
    tokio::fs::create_dir_all(&save_path).await?;
    let full_path = save_path.join(safe_file_name);
    let file = File::create(&full_path).await?;
    let mut writer = BufWriter::with_capacity(128 * 1024, file);

    // 零拷贝写入
    let mut progress_reader = ProgressReader {
        inner: stream.take(file_size),
        total: file_size,
        current: 0,
        last_reported: 0,
        on_progress: Box::new(|c, t| {
            // TODO: 进度回调
        }),
    };

    // TODO: 如果连续 30 秒没有任何数据传输，才断开
    let copy_result = timeout(
        Duration::from_secs(30),
        tokio::io::copy(&mut progress_reader, &mut writer)
    ).await;

    match copy_result {
        Ok(Ok(bytes_copied)) => {
            // flush
            writer.flush().await?;
            info!("✅ 文件接收成功: {} ({} bytes)", file_name, bytes_copied);

            Ok(())
        }
        Ok(Err(e)) => {
            // IO 错误
            Err(anyhow::anyhow!("IO Error during copy: {}", e))
        }
        Err(_) => {
            // 超时错误
            Err(anyhow::anyhow!("Transfer timed out: No data for 30s"))
        }
    }
}

pub async fn send_file_fast(dest_addr: String, file_path: String) -> anyhow::Result<()> {
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
    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    println!("{:?}", file_name);
    println!("{}", file_size);

    // 协议头: |文件名长度|文件名称|文件大小|
    stream.write_u32_le(file_name.len() as u32).await?;
    stream.write_all(file_name.as_bytes()).await?;
    stream.write_u64_le(file_size).await?;
    // 发送协议头
    stream.flush().await?;

    // 监控进度
    let mut progress_reader = ProgressReader {
        inner: file,
        total: file_size,
        current: 0,
        last_reported: 0,
        on_progress: Box::new(|c, t| {
            // TODO: 进度回调
        }),
    };

    // 零拷贝发送
    let bytes_sent = tokio::io::copy(&mut progress_reader, &mut stream).await?;

    info!("✅ 传输完成: {} ({} bytes)", file_name, bytes_sent);
    Ok(())
}


// 定义一个包装器，记录经过的字节数
struct ProgressReader<R> {
    inner: R,
    total: u64,
    current: u64,
    last_reported: u64,
    // 传入回调或 Channel
    on_progress: Box<dyn Fn(u64, u64) + Send + Sync>,
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
            self.current += n as u64;

            // 🚀 频率控制：每增加 1MB 或者传输结束时才回调
            // 1024 * 1024 = 1MB
            if self.current - self.last_reported >= 1024 * 1024 || self.current == self.total {
                self.last_reported = self.current;
                (self.on_progress)(self.current, self.total);
            }
        }
        poll
    }
}
