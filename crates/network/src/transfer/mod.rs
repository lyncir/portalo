//! PORTALO2: immutable manifests, multiplexed checksummed blocks and durable
//! receiver checkpoints. All paths on the wire are validated components.
mod metrics;
mod model;
mod preparation;
mod storage;
#[cfg(test)]
mod tests;
mod transport;
mod wire;
pub use model::{Entry, Layout, Manifest, SavedTask};
pub use preparation::prepare_streaming_with_progress;
pub use storage::{discard, prepare, prepare_with_progress, tasks};
pub(crate) use transport::receive;

pub(crate) async fn busy(stream: &mut tokio::net::TcpStream) {
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        wire::json(
            stream,
            &wire::Message::Error {
                code: "BUSY".into(),
                detail: "接收任务已满，请稍后继续".into(),
            },
        ),
    )
    .await;
}
pub use transport::send;

pub const MAGIC: &[u8; 8] = b"PORTALO2";
pub const BLOCK: usize = 4 * 1024 * 1024;
pub const FRAME: usize = 256 * 1024;
pub const STREAMS: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Command {
    #[default]
    Run,
    Pause,
    Cancel,
}

#[derive(bevy::prelude::Resource, Clone)]
pub struct TransferControl(pub tokio::sync::watch::Sender<Command>);
impl Default for TransferControl {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(Command::Run).0)
    }
}
