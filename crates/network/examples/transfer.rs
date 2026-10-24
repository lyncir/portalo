//! Headless PORTALO2 transfer: transfer receive DIR / transfer send ADDRESS FILE.
//! Use an isolated XDG_CONFIG_HOME for test task records. Failed sends remain
//! resumable through the send_task example or the normal UI.
use portalo_core::TransferProgressMsg;
use portalo_network::{SendSource, start_file_receiver, transfer};
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
            let address = args[2].parse()?;
            let path = PathBuf::from(&args[3]);
            let (_control, receiver) = watch::channel(transfer::Command::Run);
            let preparation = receiver.clone();
            let task = tokio::task::spawn_blocking(move || {
                transfer::prepare_streaming_with_progress(
                    vec![SendSource::Path(path)],
                    vec![address],
                    &preparation,
                    &|_, _| {},
                )
            })
            .await??;
            transfer::send(task, tx, 1, receiver).await?;
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
