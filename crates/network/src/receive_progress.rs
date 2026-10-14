use portalo_core::receive::{ReceivePhase, ReceiveTask, ReceiveTransfers};
use std::{net::SocketAddr, time::Instant};

pub(super) struct ReceiveProgress {
    tracked: Option<(ReceiveTransfers, ReceiveTask)>,
}

impl ReceiveProgress {
    pub fn new(source: SocketAddr, transfers: Option<ReceiveTransfers>) -> anyhow::Result<Self> {
        let tracked = match transfers {
            Some(transfers) => {
                let task = transfers
                    .begin(source)
                    .ok_or_else(|| anyhow::anyhow!("接收任务已满"))?;
                Some((transfers, task))
            }
            None => None,
        };
        Ok(Self { tracked })
    }

    fn publish(&mut self) {
        if let Some((transfers, task)) = &mut self.tracked {
            task.updated_at = Instant::now();
            transfers.update(task.clone());
        }
    }

    pub fn header(&mut self, name: &str, size: u64) {
        if let Some((_, task)) = &mut self.tracked {
            task.file_name = name.into();
            task.total = Some(size);
        }
        self.publish();
    }

    pub fn receiving(&mut self) {
        if let Some((_, task)) = &mut self.tracked {
            task.phase = ReceivePhase::Receiving;
        }
        self.publish();
    }

    pub fn advance(&mut self, bytes: u64) {
        if let Some((transfers, task)) = &mut self.tracked {
            task.received += bytes;
            transfers.record_bytes(task.id, task.received);
        }
    }

    pub fn saving(&mut self) {
        if let Some((_, task)) = &mut self.tracked {
            task.phase = ReceivePhase::Saving;
        }
        self.publish();
    }

    pub fn saved(&mut self) {
        if let Some((_, task)) = &mut self.tracked {
            task.phase = ReceivePhase::Confirming;
            task.saved_at = Some(Instant::now());
        }
        self.publish();
    }

    pub fn finish(&mut self, error: Option<String>) {
        if let Some((_, task)) = &mut self.tracked {
            task.finished_at = Some(Instant::now());
            task.phase = match error {
                None => ReceivePhase::Completed,
                Some(error) if task.saved_at.is_some() => {
                    ReceivePhase::SavedWithoutConfirmation(error)
                }
                Some(error) => ReceivePhase::Failed(error),
            };
        }
        self.publish();
    }
}

impl Drop for ReceiveProgress {
    fn drop(&mut self) {
        if self
            .tracked
            .as_ref()
            .is_some_and(|(_, task)| task.phase.is_active())
        {
            self.finish(Some("接收任务已中止".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_flushes_unreported_bytes_and_ack_failure_preserves_saved_state() {
        let ledger = ReceiveTransfers::default();
        let mut reporter =
            ReceiveProgress::new("127.0.0.1:1000".parse().unwrap(), Some(ledger.clone())).unwrap();
        reporter.header("test.bin", 100);
        reporter.receiving();
        reporter.advance(100);
        reporter.saving();
        let task = ledger.snapshot_since(None).unwrap().1.remove(0);
        assert_eq!(task.received, 100);
        assert_eq!(task.phase, ReceivePhase::Saving);
        reporter.saved();
        reporter.finish(Some("ACK failed".into()));
        drop(reporter);
        let task = ledger.snapshot_since(None).unwrap().1.remove(0);
        assert!(matches!(
            task.phase,
            ReceivePhase::SavedWithoutConfirmation(_)
        ));
        assert!(task.saved_at.is_some());
    }

    #[test]
    fn aborted_receiver_leaves_a_terminal_snapshot() {
        let ledger = ReceiveTransfers::default();
        let mut reporter =
            ReceiveProgress::new("127.0.0.1:1000".parse().unwrap(), Some(ledger.clone())).unwrap();
        reporter.header("test.bin", 100);
        reporter.receiving();
        reporter.advance(37);
        drop(reporter);
        let task = ledger.snapshot_since(None).unwrap().1.remove(0);
        assert_eq!(task.received, 37);
        assert!(matches!(task.phase, ReceivePhase::Failed(_)));
    }
}
