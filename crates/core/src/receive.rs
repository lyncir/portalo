use bevy::prelude::Resource;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const MAX_ACTIVE_RECEIVES: usize = 2;
pub const MAX_FINISHED_RECEIVES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceivePhase {
    Preparing,
    Receiving,
    Saving,
    Confirming,
    Completed,
    SavedWithoutConfirmation(String),
    Failed(String),
}

impl ReceivePhase {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Receiving | Self::Saving | Self::Confirming
        )
    }
}

#[derive(Clone, Debug)]
pub struct ReceiveTask {
    pub id: u64,
    pub file_name: String,
    pub source: SocketAddr,
    pub total: Option<u64>,
    pub received: u64,
    pub saved_bytes: Option<u64>,
    pub phase: ReceivePhase,
    pub started_at: Instant,
    pub updated_at: Instant,
    pub saved_at: Option<Instant>,
    pub finished_at: Option<Instant>,
}

impl ReceiveTask {
    pub fn progress(&self) -> f32 {
        match self.total {
            Some(0)
                if matches!(
                    self.phase,
                    ReceivePhase::Saving
                        | ReceivePhase::Confirming
                        | ReceivePhase::Completed
                        | ReceivePhase::SavedWithoutConfirmation(_)
                ) =>
            {
                1.0
            }
            Some(total) if total > 0 => {
                (self.received as f64 / total as f64).clamp(0.0, 1.0) as f32
            }
            _ => 0.0,
        }
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        self.saved_at
            .or(self.finished_at)
            .unwrap_or(now)
            .saturating_duration_since(self.started_at)
    }

    pub fn average_bytes_per_second(&self) -> f64 {
        let Some(saved) = self.saved_at else {
            return 0.0;
        };
        self.received as f64
            / saved
                .saturating_duration_since(self.started_at)
                .as_secs_f64()
                .max(0.001)
    }
}

#[derive(Default)]
struct Ledger {
    revision: u64,
    next_id: u64,
    tasks: Vec<ReceiveTask>,
}

/// Latest per-task snapshots, not a queue of progress events. Bounded to two
/// active tasks and eight terminal records; UI dismissal never cancels I/O.
#[derive(Resource, Clone, Default)]
pub struct ReceiveTransfers(Arc<Mutex<Ledger>>);

impl ReceiveTransfers {
    pub fn begin(&self, source: SocketAddr) -> Option<ReceiveTask> {
        let mut ledger = self.0.lock().expect("Receive ledger lock");
        if ledger
            .tasks
            .iter()
            .filter(|task| task.phase.is_active())
            .count()
            >= MAX_ACTIVE_RECEIVES
        {
            return None;
        }
        ledger.next_id = ledger.next_id.checked_add(1)?;
        let now = Instant::now();
        let task = ReceiveTask {
            saved_bytes: None,
            id: ledger.next_id,
            file_name: String::new(),
            source,
            total: None,
            received: 0,
            phase: ReceivePhase::Preparing,
            started_at: now,
            updated_at: now,
            saved_at: None,
            finished_at: None,
        };
        ledger.tasks.push(task.clone());
        ledger.revision += 1;
        Some(task)
    }

    pub fn update(&self, task: ReceiveTask) {
        let mut ledger = self.0.lock().expect("Receive ledger lock");
        let Some(existing) = ledger
            .tasks
            .iter_mut()
            .find(|entry| entry.id == task.id && entry.phase.is_active())
        else {
            return;
        };
        *existing = task;
        while ledger
            .tasks
            .iter()
            .filter(|task| !task.phase.is_active())
            .count()
            > MAX_FINISHED_RECEIVES
        {
            let oldest = ledger
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| !task.phase.is_active())
                .min_by_key(|(_, task)| (task.finished_at, task.id))
                .map(|(index, _)| index)
                .unwrap();
            ledger.tasks.remove(oldest);
        }
        ledger.revision += 1;
    }

    pub fn dismiss(&self, id: u64) -> bool {
        let mut ledger = self.0.lock().expect("Receive ledger lock");
        let Some(index) = ledger
            .tasks
            .iter()
            .position(|task| task.id == id && !task.phase.is_active())
        else {
            return false;
        };
        ledger.tasks.remove(index);
        ledger.revision += 1;
        true
    }

    /// Update only the byte counter in the hot I/O path, without cloning names
    /// or allocating progress messages. Revisions are throttled; the UI also
    /// samples the latest counters on its rate timer so a paused stream never
    /// hides the bytes written since the last published revision.
    pub fn record_bytes(&self, id: u64, received: u64) {
        let mut ledger = self.0.lock().expect("Receive ledger lock");
        let Some(task) = ledger
            .tasks
            .iter_mut()
            .find(|task| task.id == id && task.phase == ReceivePhase::Receiving)
        else {
            return;
        };
        if task.received >= received {
            return;
        }
        task.received = received;
        let now = Instant::now();
        if now.saturating_duration_since(task.updated_at) >= Duration::from_millis(100) {
            task.updated_at = now;
            ledger.revision += 1;
        }
    }

    pub fn snapshot_since(&self, revision: Option<u64>) -> Option<(u64, Vec<ReceiveTask>)> {
        let ledger = self.0.lock().expect("Receive ledger lock");
        (revision != Some(ledger.revision)).then(|| (ledger.revision, ledger.tasks.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> SocketAddr {
        "127.0.0.1:1000".parse().unwrap()
    }

    #[test]
    fn bounded_history_preserves_active_receives_and_dismissed_tasks_stay_gone() {
        let ledger = ReceiveTransfers::default();
        let active = ledger.begin(source()).unwrap();
        assert!(!ledger.dismiss(active.id));
        let mut last = None;
        for _ in 0..20 {
            let mut task = ledger.begin(source()).unwrap();
            assert!(ledger.begin(source()).is_none());
            task.phase = ReceivePhase::Failed("test".into());
            task.finished_at = Some(Instant::now());
            ledger.update(task.clone());
            last = Some(task);
        }
        let (revision, tasks) = ledger.snapshot_since(None).unwrap();
        assert_eq!(tasks.len(), MAX_FINISHED_RECEIVES + 1);
        assert!(
            tasks
                .iter()
                .any(|task| task.id == active.id && task.phase.is_active())
        );
        assert!(ledger.snapshot_since(Some(revision)).is_none());
        let last = last.unwrap();
        assert!(ledger.dismiss(last.id));
        ledger.update(last.clone());
        assert!(
            !ledger
                .snapshot_since(None)
                .unwrap()
                .1
                .iter()
                .any(|task| task.id == last.id)
        );
    }

    #[test]
    fn zero_byte_progress_and_average_use_save_time_not_ack_delay() {
        let ledger = ReceiveTransfers::default();
        let mut task = ledger.begin(source()).unwrap();
        task.total = Some(0);
        assert_eq!(task.progress(), 0.0);
        task.phase = ReceivePhase::Saving;
        assert_eq!(task.progress(), 1.0);
        assert!(task.phase.is_active());
        task.total = Some(1024);
        task.received = 1024;
        task.saved_at = Some(task.started_at + Duration::from_secs(2));
        task.finished_at = Some(task.started_at + Duration::from_secs(32));
        task.phase = ReceivePhase::SavedWithoutConfirmation("ACK failed".into());
        assert_eq!(task.average_bytes_per_second(), 512.0);
        assert_eq!(task.elapsed(Instant::now()), Duration::from_secs(2));
    }
}
