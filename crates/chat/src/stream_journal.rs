//! Ordered canonical journal writes with explicit terminal barriers.

use std::sync::Arc;

use {
    chelix_sessions::{PersistedMessage, store::SessionStore},
    tokio::sync::{mpsc, oneshot},
    tokio_util::sync::CancellationToken,
};

pub(crate) struct StreamJournal {
    sender: mpsc::UnboundedSender<JournalCommand>,
}

enum JournalCommand {
    Append(Box<PersistedMessage>),
    Barrier(oneshot::Sender<Result<(), String>>),
}

impl StreamJournal {
    pub(crate) fn new(
        store: Arc<SessionStore>,
        key: String,
        cancellation: CancellationToken,
    ) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut failure = None;
            while let Some(command) = receiver.recv().await {
                match command {
                    JournalCommand::Append(message) => {
                        if failure.is_some() {
                            continue;
                        }
                        if let Err(error) = store.append(&key, &message.to_value()).await {
                            tracing::error!(session_key = key, %error, "stream journal failed");
                            failure = Some(error.to_string());
                            cancellation.cancel();
                        }
                    },
                    JournalCommand::Barrier(receipt) => {
                        let _ = receipt.send(failure.clone().map_or(Ok(()), Err));
                    },
                }
            }
        });
        Self { sender }
    }

    pub(crate) fn append(&self, message: PersistedMessage) -> Result<(), String> {
        self.sender
            .send(JournalCommand::Append(Box::new(message)))
            .map_err(|error| format!("stream journal is closed: {error}"))
    }

    pub(crate) async fn flush(&self) -> Result<(), String> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(JournalCommand::Barrier(sender))
            .map_err(|error| format!("stream journal is closed: {error}"))?;
        receiver
            .await
            .map_err(|error| format!("stream journal receipt failed: {error}"))?
    }
}
