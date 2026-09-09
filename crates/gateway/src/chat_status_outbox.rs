//! Latest pending chat status per session and independent status stream.

use std::{collections::BTreeMap, sync::Mutex};

use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChatStatusStream {
    Run,
    Queue,
    Compaction,
    Voice,
}

#[derive(Debug)]
struct PendingStatus {
    sequence: u64,
    frame: String,
}

#[derive(Debug, Default)]
pub struct ChatStatusOutbox {
    pending: Mutex<BTreeMap<(String, ChatStatusStream), PendingStatus>>,
    changed: Notify,
}

impl ChatStatusOutbox {
    pub fn publish(
        &self,
        session_key: &str,
        stream: ChatStatusStream,
        sequence: u64,
        frame: String,
    ) -> Result<(), String> {
        let mut pending = self.pending.lock().map_err(|error| error.to_string())?;
        let key = (session_key.to_string(), stream);
        if pending
            .get(&key)
            .is_none_or(|previous| previous.sequence < sequence)
        {
            pending.insert(key, PendingStatus { sequence, frame });
        }
        drop(pending);
        self.changed.notify_one();
        Ok(())
    }

    pub async fn next(&self) -> Result<String, String> {
        loop {
            let changed = self.changed.notified();
            {
                let mut pending = self.pending.lock().map_err(|error| error.to_string())?;
                let key = pending
                    .iter()
                    .min_by_key(|(_, status)| status.sequence)
                    .map(|(key, _)| key.clone());
                if let Some(key) = key
                    && let Some(status) = pending.remove(&key)
                {
                    return Ok(status.frame);
                }
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn voice_status_survives_thinking_and_precedes_terminal_status() -> Result<(), String> {
        for run_status in ["thinking", "final", "error", "aborted"] {
            let outbox = ChatStatusOutbox::default();
            outbox.publish("main", ChatStatusStream::Voice, 1, "voice_pending".into())?;
            outbox.publish("main", ChatStatusStream::Run, 2, run_status.into())?;
            assert_eq!(outbox.next().await?, "voice_pending");
            assert_eq!(outbox.next().await?, run_status);
        }
        Ok(())
    }

    #[tokio::test]
    async fn slow_writer_retains_latest_independent_statuses() -> Result<(), String> {
        let outbox = ChatStatusOutbox::default();
        for sequence in 1..=1000 {
            outbox.publish(
                "main",
                ChatStatusStream::Run,
                sequence,
                sequence.to_string(),
            )?;
        }
        outbox.publish("main", ChatStatusStream::Queue, 1001, "queue".into())?;
        outbox.publish("other", ChatStatusStream::Run, 1002, "other".into())?;
        assert_eq!(outbox.next().await?, "1000");
        assert_eq!(outbox.next().await?, "queue");
        assert_eq!(outbox.next().await?, "other");
        Ok(())
    }
}
