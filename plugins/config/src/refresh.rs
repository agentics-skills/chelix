//! Reread `chelix.toml` every 60 seconds until stop.

use chelix_call_bus::guest::Host;
use futures::channel::oneshot;
use futures::future::{select, Either};

use crate::snapshot::ConfigState;

pub fn spawn(state: ConfigState, done: oneshot::Sender<()>) {
    let (stop_tx, mut stop_rx) = oneshot::channel();
    *state.shutdown.refresh_stop.borrow_mut() = Some(stop_tx);
    chelix_call_bus::guest::spawn_local(async move {
        let host = Host;
        loop {
            let sleep = Box::pin(host.sleep(60_000));
            match select(sleep, &mut stop_rx).await {
                Either::Left(_) => {
                    if *state.shutdown.flag.borrow() {
                        break;
                    }
                    state.queue.enqueue_refresh();
                },
                Either::Right(_) => break,
            }
        }
        let _ = done.send(());
    });
}
