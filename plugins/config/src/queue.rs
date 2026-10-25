//! Serial patch and refresh transactions.

use std::cell::RefCell;
use std::rc::Rc;

use chelix_call_bus::guest::{spawn_local, Host, PluginContext};
use chelix_call_bus::jsonrpc::RpcError;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot;
use futures::StreamExt;
use serde_json::Value;
use toml_edit::DocumentMut;

use crate::contract::{self, ConfigPatchParams, ConfigPatchResult, INVALID_CANDIDATE, IO};
use crate::patch;
use crate::snapshot::{self, ConfigState, Snapshot};
use crate::validate::{self, Candidate, TomlCheck};

#[derive(Clone)]
pub struct QueueHandle {
    tx: UnboundedSender<Job>,
}

#[derive(Clone)]
pub struct Shutdown {
    pub(crate) flag: Rc<RefCell<bool>>,
    pub(crate) refresh_stop: Rc<RefCell<Option<oneshot::Sender<()>>>>,
    pub(crate) queue_done: Rc<RefCell<Option<oneshot::Receiver<()>>>>,
    pub(crate) refresh_done: Rc<RefCell<Option<oneshot::Receiver<()>>>>,
}

pub(crate) enum Job {
    Patch {
        params: ConfigPatchParams,
        reply: oneshot::Sender<Result<ConfigPatchResult, RpcError>>,
    },
    Refresh,
}

impl QueueHandle {
    pub fn enqueue_patch(
        &self,
        params: ConfigPatchParams,
    ) -> Result<oneshot::Receiver<Result<ConfigPatchResult, RpcError>>, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .unbounded_send(Job::Patch { params, reply: tx })
            .map_err(|_| RpcError::new(IO, "queue is closed").with_data(serde_json::json!({"op": "read"})))?;
        Ok(rx)
    }

    pub fn enqueue_refresh(&self) {
        let _ = self.tx.unbounded_send(Job::Refresh);
    }

    pub fn close_channel(&self) {
        self.tx.close_channel();
    }
}

pub fn open() -> (
    QueueHandle,
    UnboundedReceiver<Job>,
    Shutdown,
    oneshot::Sender<()>,
    oneshot::Sender<()>,
) {
    let (tx, rx) = mpsc::unbounded();
    let (queue_done_tx, queue_done_rx) = oneshot::channel();
    let (refresh_done_tx, refresh_done_rx) = oneshot::channel();
    (
        QueueHandle { tx },
        rx,
        Shutdown {
            flag: Rc::new(RefCell::new(false)),
            refresh_stop: Rc::new(RefCell::new(None)),
            queue_done: Rc::new(RefCell::new(Some(queue_done_rx))),
            refresh_done: Rc::new(RefCell::new(Some(refresh_done_rx))),
        },
        queue_done_tx,
        refresh_done_tx,
    )
}

pub fn spawn(
    state: ConfigState,
    host: Host,
    mut rx: UnboundedReceiver<Job>,
    done: oneshot::Sender<()>,
) {
    spawn_local(async move {
        let checks: Vec<Box<dyn validate::CandidateCheck>> = vec![Box::new(TomlCheck)];
        while let Some(job) = rx.next().await {
            match job {
                Job::Patch { params, reply } => {
                    let outcome = run_patch(&state, &host, &checks, params).await;
                    let after = outcome.as_ref().ok().and_then(|item| item.after.clone());
                    let _ = reply.send(outcome.map(|item| item.result));
                    if let Some(after) = after {
                        publish_after(&host, &after).await;
                    }
                },
                Job::Refresh => {
                    let _ = run_refresh(&state, &host).await;
                },
            }
        }
        let _ = done.send(());
    });
}

struct PatchOutcome {
    result: ConfigPatchResult,
    after: Option<AfterPublish>,
}

#[derive(Clone)]
struct AfterPublish {
    revision: u64,
    sections: Vec<String>,
    changed: contract::ConfigChanged,
}

async fn publish_after(host: &Host, after: &AfterPublish) {
    if let Err(error) = host
        .publish(
            contract::CHANGED,
            &after.changed,
            chelix_call_bus::guest::chelix::plugin::host::Delivery::Queue,
        )
        .await
    {
        host.log_error(&format!(
            "config.changed revision {}: {error}",
            after.revision
        ));
    }
    if after.sections.iter().any(|section| section == "plugins") {
        let host = Host;
        let revision = after.revision;
        spawn_local(async move {
            if let Err(error) = host.call(contract::RELOAD, &serde_json::json!({})).await {
                host.log_error(&format!("bus.plugins.reload revision {revision}: {error}"));
            }
        });
    }
}

async fn run_patch(
    state: &ConfigState,
    host: &Host,
    checks: &[Box<dyn validate::CandidateCheck>],
    params: ConfigPatchParams,
) -> Result<PatchOutcome, RpcError> {
    let bytes = host
        .read("config_file", "")
        .await
        .map_err(|error| io_error(&state.file, "read", &error))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| invalid_candidate(&state.file, &error.to_string()))?;
    let mut document: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| invalid_candidate(&state.file, &error.to_string()))?;
    let sections = patch::apply(&mut document, &params.ops, &state.file)?;
    let candidate_text = document.to_string();
    if candidate_text == text {
        return Ok(PatchOutcome {
            result: ConfigPatchResult {
                revision: snapshot::current(state).revision,
                changed: false,
            },
            after: None,
        });
    }
    let table = toml::from_str::<toml::Table>(&candidate_text)
        .map_err(|error| invalid_candidate(&state.file, &error.to_string()))?;
    let candidate = Candidate {
        text: candidate_text.clone(),
        table: table.clone(),
        changed_sections: sections.clone(),
    };
    if let Err(error) = validate::run(checks, &candidate).await {
        return Err(RpcError::new(
            INVALID_CANDIDATE,
            format!("{}: {}", state.file, error.message),
        )
        .with_data(serde_json::json!({"check": error.name})));
    }
    host.atomic_write("config_file", "", candidate_text.as_bytes())
        .await
        .map_err(|error| io_error(&state.file, "atomic-write", &error))?;
    let revision = snapshot::publish(state, table);
    Ok(PatchOutcome {
        result: ConfigPatchResult {
            revision,
            changed: true,
        },
        after: Some(AfterPublish {
            revision,
            sections: sections.clone(),
            changed: contract::ConfigChanged {
                revision,
                sections,
            },
        }),
    })
}

async fn run_refresh(state: &ConfigState, host: &Host) -> Result<(), ()> {
    let bytes = match host.read("config_file", "").await {
        Ok(bytes) => bytes,
        Err(error) => {
            host.log_error(&format!("{}: {error}", state.file));
            return Err(());
        },
    };
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            host.log_error(&format!("{}: {error}", state.file));
            return Err(());
        },
    };
    let table = match toml::from_str::<toml::Table>(&text) {
        Ok(table) => table,
        Err(error) => {
            host.log_error(&format!("{}: {error}", state.file));
            return Err(());
        },
    };
    let current = snapshot::current(state);
    if current.root == table {
        return Ok(());
    }
    let revision = snapshot::publish(state, table.clone());
    let sections = diff_sections(&current.root, &table);
    let changed = contract::ConfigChanged { revision, sections: sections.clone() };
    if let Err(error) = host
        .publish(contract::CHANGED, &changed, chelix_call_bus::guest::chelix::plugin::host::Delivery::Queue)
        .await
    {
        host.log_error(&format!("config.changed revision {revision}: {error}"));
    }
    if sections.iter().any(|section| section == "plugins") {
        let host = Host;
        spawn_local(async move {
            if let Err(error) = host.call(contract::RELOAD, &serde_json::json!({})).await {
                host.log_error(&format!("bus.plugins.reload revision {revision}: {error}"));
            }
        });
    }
    Ok(())
}

fn diff_sections(before: &toml::Table, after: &toml::Table) -> Vec<String> {
    let mut sections = Vec::new();
    for (key, value) in after {
        if before.get(key) != Some(value) {
            sections.push(key.clone());
        }
    }
    for key in before.keys() {
        if !after.contains_key(key) {
            sections.push(key.clone());
        }
    }
    sections
}

fn io_error(file: &str, op: &str, error: &str) -> RpcError {
    RpcError::new(contract::IO, format!("{file}: {error}")).with_data(serde_json::json!({"op": op}))
}

fn invalid_candidate(file: &str, message: &str) -> RpcError {
    RpcError::new(INVALID_CANDIDATE, format!("{file}: {message}"))
        .with_data(serde_json::json!({"check": "toml"}))
}

pub async fn patch(
    ctx: &PluginContext<ConfigState>,
    params: ConfigPatchParams,
) -> Result<ConfigPatchResult, RpcError> {
    let state = ctx.state();
    let rx = state.queue.enqueue_patch(params)?;
    rx.await
        .map_err(|_| RpcError::new(contract::IO, format!("{}: queue closed", state.file)))?
}

pub fn extension() -> Value {
    serde_json::json!({
        "x-chelix-plugin": {
            "requires": {},
            "resources": [{
                "id": "config_file",
                "kind": "file",
                "access": "read_write",
                "create": true,
                "file_mode": "0600"
            }],
            "subscribes": [],
            "provides": [{ "point": "bus.activation", "order": 0, "requires": [], "meta": {} }]
        },
        "x-chelix-points": [],
        "x-chelix-events": [{
            "name": "changed",
            "description": "A new snapshot was published.",
            "payload": { "type": "object" }
        }]
    })
}
