use bevy::prelude::*;
use portalo_core::ReceiveFeedback;
use portalo_core::receive::{MAX_ACTIVE_RECEIVES, ReceiveTransfers};
use portalo_core::settings::{ReceiveDestination, SharedSettings};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    runtime::Runtime,
    sync::{Semaphore, watch},
    time::timeout,
};

const IDLE: Duration = Duration::from_secs(30);

pub struct NetworkPlugin;
#[derive(Resource)]
pub struct TokioRuntime(pub Runtime);
#[derive(Resource)]
struct ReceiveNoticeSender(watch::Sender<String>);
mod connect;
mod filename;
mod receive_progress;
mod source;
pub mod transfer;
#[cfg(windows)]
mod windows_io;
pub use source::SendSource;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod kernel_copy;
use receive_progress::ReceiveProgress;

impl Plugin for NetworkPlugin {
    fn build(&self, app: &mut App) {
        let (notice_tx, notice_rx) = watch::channel(String::new());
        app.init_resource::<SharedSettings>()
            .init_resource::<ReceiveTransfers>()
            .insert_resource(ReceiveNoticeSender(notice_tx))
            .insert_resource(ReceiveFeedback(notice_rx))
            .insert_resource(TokioRuntime(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create Tokio runtime"),
            ))
            .add_systems(Startup, setup_network_listener);
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn buffer_size() -> usize {
    std::env::var("PORTALO_BUFFER_KIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| (64..=1024).contains(n))
        .unwrap_or(256)
        * 1024
}

fn setup_network_listener(
    runtime: Res<TokioRuntime>,
    settings: Res<SharedSettings>,
    notices: Res<ReceiveNoticeSender>,
    transfers: Res<ReceiveTransfers>,
) {
    let settings = settings.clone();
    let notices = notices.0.clone();
    let transfers = transfers.clone();
    runtime.0.spawn(async move {
        let result: anyhow::Result<()> = async {
            let listener = TcpListener::bind("0.0.0.0:49527").await?;
            let slots = Arc::new(Semaphore::new(MAX_ACTIVE_RECEIVES));
            loop {
                let (mut stream, addr) = listener.accept().await?;
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    transfer::busy(&mut stream).await;
                    continue;
                };
                let destination = settings
                    .0
                    .read()
                    .expect("Settings lock")
                    .receive_destination();
                let transfers = transfers.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) =
                        start_file_receiver_tracked(stream, destination, transfers).await
                    {
                        error!("Receive from {addr}: {error:#}");
                    }
                });
            }
        }
        .await;
        if let Err(error) = result {
            error!("Network listener: {error:#}");
            notices.send_replace(format!("无法接收文件：{error:#}"));
        }
    });
}

pub async fn start_file_receiver(stream: TcpStream, directory: PathBuf) -> anyhow::Result<()> {
    start_file_receiver_with_destination(stream, ReceiveDestination::Directory(directory)).await
}

pub async fn start_file_receiver_with_destination(
    stream: TcpStream,
    destination: ReceiveDestination,
) -> anyhow::Result<()> {
    receive_impl(stream, destination, None).await
}

pub async fn start_file_receiver_tracked(
    stream: TcpStream,
    destination: ReceiveDestination,
    transfers: ReceiveTransfers,
) -> anyhow::Result<()> {
    receive_impl(stream, destination, Some(transfers)).await
}

async fn receive_impl(
    mut stream: TcpStream,
    destination: ReceiveDestination,
    transfers: Option<ReceiveTransfers>,
) -> anyhow::Result<()> {
    let mut progress = ReceiveProgress::new(stream.peer_addr()?, transfers)?;
    let header = async {
        let mut magic = [0; 8];
        timeout(IDLE, stream.read_exact(&mut magic)).await??;
        anyhow::ensure!(
            &magic == transfer::MAGIC,
            "Unsupported protocol; update both devices"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = header {
        progress.finish(Some(format!("{error:#}")));
        return Err(error);
    }
    transfer::receive(stream, destination, progress).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use portalo_core::receive::ReceivePhase;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn unsupported_protocol_is_rejected_without_creating_output() {
        for magic in [b"PORTALO1", b"INVALID!"] {
            let target = tempfile::tempdir().unwrap();
            let directory = target.path().join("receive");
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            client.write_all(magic).await.unwrap();
            let transfers = ReceiveTransfers::default();
            let error = start_file_receiver_tracked(
                stream,
                ReceiveDestination::Directory(directory.clone()),
                transfers.clone(),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("Unsupported protocol"));
            assert!(!directory.exists());
            let tasks = transfers.snapshot_since(None).unwrap().1;
            assert!(
                matches!(&tasks[0].phase, ReceivePhase::Failed(message) if message.contains("Unsupported protocol"))
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn incomplete_header_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        client.write_all(b"PORT").await.unwrap();
        let target = tempfile::tempdir().unwrap();
        let result = start_file_receiver(stream, target.path().to_owned()).await;
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
    }
}
