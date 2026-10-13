//! Headless real-protocol benchmark: transfer receive DIR / transfer send ADDRESS FILE
use portalo_core::TransferProgressMsg;
use portalo_network::{send_file_fast, start_file_receiver};
use std::{path::PathBuf, time::Instant};
use tokio::{net::TcpListener, sync::watch};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("receive") if args.len() == 3 => {
            let listener = TcpListener::bind("0.0.0.0:49527").await?;
            loop {
                let (stream, _) = listener.accept().await?;
                let start = Instant::now();
                start_file_receiver(stream, PathBuf::from(&args[2])).await?;
                println!(
                    "receive-confirmed elapsed={:.3}s",
                    start.elapsed().as_secs_f64()
                );
            }
        }
        Some("send") if args.len() == 4 => {
            let (tx, _rx) = watch::channel(TransferProgressMsg::default());
            let size = tokio::fs::metadata(&args[3]).await?.len();
            let start = Instant::now();
            send_file_fast(vec![args[2].parse()?], &args[3], tx, 1).await?;
            let seconds = start.elapsed().as_secs_f64();
            println!(
                "bytes={size} confirmed_seconds={seconds:.3} MiB/s={:.3}",
                size as f64 / 1_048_576.0 / seconds
            );
        }
        _ => anyhow::bail!("Usage: transfer receive DIRECTORY | transfer send IP:PORT FILE"),
    }
    Ok(())
}
