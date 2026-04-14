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
    pub bytes_transferred: u64, // 已传输大小
    pub total_bytes: u64,       // 总大小
    // 用于计算速度
    pub last_update_time: f64, // 上次计算的时间
    pub last_bytes: u64,       // 上次计算时的字节数
    pub current_speed: f32,    // 最终显示的速度
}

impl FileTransferState {
    pub fn new(total_bytes: u64) -> Self {
        Self {
            bytes_transferred: 0,
            total_bytes,
            last_update_time: 0.0,
            last_bytes: 0,
            current_speed: 0.0,
        }
    }

    // 当前进度
    pub fn get_progress(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.bytes_transferred as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
        }
    }

    // 是否完成
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
