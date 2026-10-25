//! Plugin declaration for `config`.

use std::cell::RefCell;
use std::rc::Rc;

use chelix_call_bus::guest::Host;
use chelix_call_bus::guest::plugin;
use toml::Table;

use crate::contract::{self, ActivationParams, ActivationResult, ConfigGetParams, ConfigGetResult, ConfigPatchParams, ConfigPatchResult};
use crate::location;
use crate::queue::{self, patch};
use crate::refresh;
use crate::snapshot::{self, activation, get, ConfigState, Snapshot};

plugin! {
    name: "config",
    api: "config",
    version: env!("CARGO_PKG_VERSION"),
    description: "Owner of chelix.toml: snapshot reads and transactional patches.",
    state: ConfigState,
    protocol: 1,
    extension: queue::extension(),
    methods: [
        "get", ConfigGetParams, ConfigGetResult, [(contract::INVALID_PATH, "path is not a TOML key")], contract::get_example(), get,
        "patch", ConfigPatchParams, ConfigPatchResult, [(contract::INVALID_PATH, "path is not a TOML key"), (contract::INVALID_VALUE, "value is not TOML"), (contract::PATH_CONFLICT, "path meets a non-table"), (contract::INVALID_CANDIDATE, "candidate failed a check"), (contract::IO, "config file io failed")], contract::patch_example(), patch,
    ],
    calls: [
        "bus.activation", ActivationParams, ActivationResult, activation,
    ],
    start: start,
    stop: stop,
}

async fn start(host: &Host) -> Result<ConfigState, String> {
    let path = location::from_env()?;
    host.bind("config_file", &path.to_string_lossy()).await?;
    let bytes = host.read("config_file", "").await?;
    let text = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    let root: Table = toml::from_str(&text).map_err(|error| format!("config: {error}"))?;
    let (queue_handle, rx, shutdown, queue_done, refresh_done) = queue::open();
    let state = ConfigState {
        file: path.display().to_string(),
        current: Rc::new(RefCell::new(Rc::new(Snapshot { revision: 1, root }))),
        queue: queue_handle,
        shutdown,
    };
    queue::spawn(state.clone(), Host, rx, queue_done);
    refresh::spawn(state.clone(), refresh_done);
    Ok(state)
}

async fn stop(_host: &Host, state: Option<&ConfigState>) {
    let Some(state) = state else {
        return;
    };
    *state.shutdown.flag.borrow_mut() = true;
    if let Some(stop) = state.shutdown.refresh_stop.borrow_mut().take() {
        let _ = stop.send(());
    }
    state.queue.close_channel();
    if let Some(done) = state.shutdown.queue_done.borrow_mut().take() {
        let _ = done.await;
    }
    if let Some(done) = state.shutdown.refresh_done.borrow_mut().take() {
        let _ = done.await;
    }
}
