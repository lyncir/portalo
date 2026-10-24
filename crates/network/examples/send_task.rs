//! Headless transfer smoke test using the production snapshot/transport path.
//! Use an isolated XDG_CONFIG_HOME for test data. A failed task remains resumable
//! from the normal UI; successful snapshots are cleaned by the transport.
use portalo_core::TransferProgressMsg;
use portalo_network::{SendSource, transfer};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let address = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Usage: send_task IP:PORT PATH [PATH ...]"))?
        .parse()?;
    let arguments: Vec<_> = args.collect();
    anyhow::ensure!(
        !arguments.is_empty(),
        "Missing source path or --resume TASK_ID"
    );
    let (_control, receiver) = watch::channel(transfer::Command::Run);
    let preparation = receiver.clone();
    let task = tokio::task::spawn_blocking(move || {
        if arguments.first().is_some_and(|arg| arg == "--resume") {
            anyhow::ensure!(arguments.len() == 2, "Expected --resume TASK_ID");
            let mut task = transfer::tasks()?
                .into_iter()
                .find(|task| task.manifest.id == arguments[1])
                .ok_or_else(|| anyhow::anyhow!("Task not found"))?;
            task.addresses = vec![address];
            return Ok(task);
        }
        let sources = arguments
            .into_iter()
            .map(|path| SendSource::Path(path.into()))
            .collect();
        transfer::prepare_streaming_with_progress(sources, vec![address], &preparation, &|_, _| {})
    })
    .await??;
    println!("Task {}: {} bytes", task.manifest.id, task.manifest.total());
    let (progress, mut updates) = watch::channel(TransferProgressMsg::default());
    let reporter = tokio::spawn(async move {
        let mut last = std::time::Instant::now();
        let mut status = portalo_core::TransferStatus::Idle;
        while updates.changed().await.is_ok() {
            let update = updates.borrow_and_update().clone();
            if last.elapsed().as_secs() >= 5
                || update.status != status
                || update.status == portalo_core::TransferStatus::AwaitingConfirmation
            {
                println!(
                    "{:?}: {}/{} {}",
                    update.status, update.current, update.total, update.detail
                );
                status = update.status;
                last = std::time::Instant::now();
            }
        }
    });
    let result = transfer::send(task, progress, 1, receiver).await;
    reporter.await?;
    result?;
    Ok(())
}
