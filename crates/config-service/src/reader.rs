use std::sync::Arc;

use {arc_swap::ArcSwap, tokio::sync::mpsc};

use crate::snapshot::Snapshot;

pub(crate) type Request = Box<dyn FnOnce(&Snapshot) + Send>;

pub(crate) async fn run(snapshot: Arc<ArcSwap<Snapshot>>, mut requests: mpsc::Receiver<Request>) {
    while let Some(select) = requests.recv().await {
        select(&snapshot.load());
        #[cfg(feature = "metrics")]
        chelix_metrics::counter!("config_reads_total").increment(1);
    }
}
