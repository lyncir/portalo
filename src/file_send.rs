use bevy::prelude::*;
use futures_util::StreamExt;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream; // 用于处理 stream

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
