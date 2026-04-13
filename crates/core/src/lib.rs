use bevy::prelude::*;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash, States)]
pub enum AppState {
    #[default]
    Menu, // 主界面
    Transfer, // 传输界面
    Setting,  // 设置界面
}

// --------------- RESOURCES --------------- //
#[derive(Resource, Default)]
pub struct FileTransferState {
    pub bytes_transferred: u64,                 // 已传输大小
    pub total_bytes: u64,                       // 总大小
    pub start_time: Option<std::time::Instant>, // 开始时间
}

impl FileTransferState {
    pub fn new(total_bytes: u64) -> Self {
        Self {
            bytes_transferred: 0,
            total_bytes,
            start_time: Some(std::time::Instant::now()),
        }
    }

    pub fn get_progress(&self) -> f32 {
        (self.bytes_transferred as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    pub fn get_speed_mbps(&self) -> f32 {
        if let Some(start_time) = self.start_time {
            let elapsed = start_time.elapsed().as_secs_f32();
            if elapsed > 0.1 {
                (self.bytes_transferred as f32 / elapsed) / 1_000_000.0 // 转换为 MB/s
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    pub fn is_complete(&self) -> bool {
        self.bytes_transferred >= self.total_bytes
    }
}

pub struct TransferProgressMsg {
    pub current: u64,
    pub total: u64,
}

#[derive(Resource)]
pub struct ProgressSender(pub mpsc::UnboundedSender<TransferProgressMsg>);

#[derive(Resource)]
pub struct ProgressReceiver(pub mpsc::UnboundedReceiver<TransferProgressMsg>);
