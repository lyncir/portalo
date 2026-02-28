use bevy::prelude::*;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};

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
