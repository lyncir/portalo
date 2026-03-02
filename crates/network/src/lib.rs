use anyhow;
use bevy::prelude::*;
use futures_util::StreamExt;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::runtime::Runtime;
use tokio_util::io::ReaderStream;

// 网络插件
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

fn setup_network_listener(runtime: Res<TokioRuntime>, // 注入我们存好的运行时
) {
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

#[derive(Resource)]
pub struct TokioRuntime(pub Runtime);

// 接收任务的逻辑
pub async fn start_file_receiver(
    mut stream: tokio::net::TcpStream,
    save_path: PathBuf,
) -> anyhow::Result<()> {
    // 1. 设置缓冲区优化 (针对移动端/桌面端平衡)

    // 2. 解析协议头 (按照我们之前的约定)
    // 读取文件名长度 (u32)
    let name_len = stream.read_u32().await? as usize;
    // 防御：防止文件名长度异常导致内存溢出
    if name_len > 4096 {
        return Err(anyhow::anyhow!("File name too long: {}", name_len));
    }

    let mut name_buf = vec![0u8; name_len];
    stream.read_exact(&mut name_buf).await?;
    let file_name = String::from_utf8_lossy(&name_buf).to_string();
    // 过滤文件名，防止路径穿越攻击（安全重点！）
    let safe_file_name = std::path::Path::new(&file_name)
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid file name"))?;

    // 读取文件总大小 (u64)
    let file_size = stream.read_u64().await?;

    println!("{:?}", safe_file_name);
    println!("{}", file_size);

    // 3. 创建文件并准备写入
    tokio::fs::create_dir_all(&save_path).await?;
    let full_path = save_path.join(&safe_file_name);

    let file = File::create(&full_path).await?;
    // 使用 BufWriter 极大提升写入机械硬盘或移动端 Flash 的速度
    let mut writer = BufWriter::with_capacity(128 * 1024, file);

    // 4. 【核心：高性能流转】
    // 使用 tokio::io::copy 直接在内核级将数据从 Socket 拼接到 File
    // 这是 Rust 中最快且最省内存的写法
    let mut reader = stream.take(file_size); // 确保只读取指定大小的内容
    let bytes_copied = tokio::io::copy(&mut reader, &mut writer).await?;

    // 强制刷新缓冲区到磁盘
    writer.flush().await?;

    info!("✅ 文件接收成功: {} ({} bytes)", file_name, bytes_copied);
    Ok(())
}

pub async fn send_file_fast(dest_addr: String, file_path: String) -> anyhow::Result<()> {
    // 1. 建立连接 (Dukto 默认端口 49527)
    let mut stream = tokio::net::TcpStream::connect(&dest_addr).await?;

    // 优化：设置 TCP_NODELAY 减少延迟，适合小块数据
    stream.set_nodelay(true)?;

    // 2. 打开文件并获取元数据
    let file = File::open(&file_path).await?;

    let metadata = file.metadata().await?;
    let file_size = metadata.len();
    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    println!("{:?}", file_name);
    println!("{}", file_size);

    // 3. 协议头：发送文件名长度、名称和大小
    // 提示：这部分需要和你定义的接收端协议对齐
    stream.write_u32(file_name.len() as u32).await?;
    stream.write_all(file_name.as_bytes()).await?;
    stream.write_u64(file_size).await?;
    stream.flush().await?;

    // 4. 【核心：高性能传输】使用 ReaderStream
    let mut reader_stream = ReaderStream::new(file);

    while let Some(chunk) = reader_stream.next().await {
        let bytes = chunk?;
        stream.write_all(&bytes).await?;

        // 这里可以计算已发送字节数，发送给 Bevy UI 更新进度条
    }

    info!("✅ 传输完成: {}", file_name);
    Ok(())
}
