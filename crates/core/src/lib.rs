use bevy::prelude::*;
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash, States)]
pub enum AppState {
    #[default]
    Menu,
    Send,
    Transfer,
    Setting,
    Connection,
    Licenses,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum TransferStatus {
    #[default]
    Idle,
    Connecting,
    Sending,
    AwaitingConfirmation,
    Complete,
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct TransferProgressMsg {
    pub id: u64,
    pub current: u64,
    pub total: u64,
    pub status: TransferStatus,
}

#[derive(Resource, Default)]
pub struct FileTransferState {
    pub id: u64,
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub last_update_time: f64,
    pub last_bytes: u64,
    pub current_speed: f32,
    pub status: TransferStatus,
}

impl FileTransferState {
    pub fn new(total_bytes: u64) -> Self {
        Self {
            total_bytes,
            ..default()
        }
    }
    pub fn get_progress(&self) -> f32 {
        if self.total_bytes == 0 {
            if self.is_complete() { 1.0 } else { 0.0 }
        } else {
            (self.bytes_transferred as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
        }
    }
    pub fn is_complete(&self) -> bool {
        self.status == TransferStatus::Complete
    }
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            TransferStatus::Connecting
                | TransferStatus::Sending
                | TransferStatus::AwaitingConfirmation
        )
    }
}

#[derive(Resource)]
pub struct ProgressSender(pub watch::Sender<TransferProgressMsg>);
#[derive(Resource)]
pub struct ProgressReceiver(pub watch::Receiver<TransferProgressMsg>);

#[derive(Resource)]
pub struct ReceiveFeedback(pub watch::Receiver<String>);

#[cfg(target_os = "android")]
pub mod android_storage;
pub mod device;
pub mod receive;
pub mod settings;
