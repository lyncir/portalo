//! Repeatable loopback throughput smoke test. Not a Wi-Fi or cold-disk benchmark.
use portalo_core::TransferProgressMsg;
use portalo_network::{send_file_fast, start_file_receiver};
use std::time::Instant;
use tokio::{net::TcpListener, sync::watch};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    let path = source.path().join("payload.bin");
    let bytes: Vec<u8> = (0..64 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    tokio::fs::write(&path, &bytes).await?;
    let mut speeds = Vec::new();
    for round in 0..5 {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let destination = target.path().to_path_buf();
        let receiving = tokio::spawn(async move {
            start_file_receiver(listener.accept().await?.0, destination).await
        });
        let (tx, _rx) = watch::channel(TransferProgressMsg::default());
        let started = Instant::now();
        send_file_fast(vec![address], &path, tx, round).await?;
        receiving.await??;
        let seconds = started.elapsed().as_secs_f64();
        let speed = 64.0 / seconds;
        speeds.push(speed);
        // Verification is outside the timed interval; exact byte comparison.
        let output = target.path().join("payload.bin");
        anyhow::ensure!(tokio::fs::read(&output).await? == bytes, "Content mismatch");
        tokio::fs::remove_file(output).await?;
        println!(
            "round={} bytes={} confirmed_seconds={seconds:.4} MiB/s={speed:.2}",
            round + 1,
            bytes.len()
        );
    }
    speeds.sort_by(f64::total_cmp);
    println!(
        "median_MiB/s={:.2} buffer_KiB={}",
        speeds[2],
        std::env::var("PORTALO_BUFFER_KIB").unwrap_or_else(|_| "256".into())
    );
    Ok(())
}
