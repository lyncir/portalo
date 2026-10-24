//! Repeatable loopback throughput smoke test. Not a Wi-Fi or cold-disk benchmark.
//! Uses PORTALO2 preparation/checkpoints; use an isolated XDG_CONFIG_HOME for
//! the small durable task records retained after each completed transfer.
use portalo_core::TransferProgressMsg;
use portalo_network::{SendSource, start_file_receiver, transfer};
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
        let source = path.clone();
        let (_control, receiver) = watch::channel(transfer::Command::Run);
        let preparation = receiver.clone();
        let task = tokio::task::spawn_blocking(move || {
            transfer::prepare_streaming_with_progress(
                vec![SendSource::Path(source)],
                vec![address],
                &preparation,
                &|_, _| {},
            )
        })
        .await??;
        transfer::send(task, tx, round, receiver).await?;
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
