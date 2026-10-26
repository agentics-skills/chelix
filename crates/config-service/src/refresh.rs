use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{mpsc, oneshot};

use crate::{
    Result,
    validation::ValidationScope,
    writer::{Command, WriteOperation, Writer, record},
};

#[cfg(test)]
impl crate::ConfigService {
    pub(crate) async fn refresh(&self) -> Result<()> {
        let (reply, completed) = oneshot::channel();
        self.writes
            .send(Box::new(ReloadFromDisk { reply }))
            .await
            .map_err(|_| crate::service::closed())?;
        completed.await.map_err(|_| crate::service::closed())?
    }
}

pub(crate) fn start(sender: mpsc::WeakSender<Command>) {
    tokio::spawn(async move {
        let period = Duration::from_secs(60);
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let Some(sender) = sender.upgrade() else {
                break;
            };
            let (reply, completed) = oneshot::channel();
            if sender
                .send(Box::new(ReloadFromDisk { reply }))
                .await
                .is_err()
            {
                break;
            }
            drop(sender);
            if completed.await.is_err() {
                break;
            }
        }
    });
}

pub(crate) struct ReloadFromDisk {
    pub(crate) reply: oneshot::Sender<Result<()>>,
}

#[async_trait::async_trait]
impl WriteOperation for ReloadFromDisk {
    async fn execute(self: Box<Self>, writer: &Writer) {
        let started = Instant::now();
        let result = reload(writer).await;
        record("refresh", &result, started);
        let _ = self.reply.send(result);
    }
}

async fn reload(writer: &Writer) -> Result<()> {
    let raw = writer.storage.read().await?;
    let candidate = writer
        .validation
        .admit(raw, &writer.storage.path, ValidationScope::Load, None)
        .await?;
    if candidate.raw != writer.snapshot.load().raw {
        writer.snapshot.store(Arc::new(candidate));
    }
    Ok(())
}
