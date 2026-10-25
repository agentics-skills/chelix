//! `CallBus` public API and the supervisor task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::document::{parse_document, PluginDocument};
use crate::info::{PluginInfo, PluginStatus, ProviderInfo};
use crate::jsonrpc::{self, RpcError};
use crate::metrics;
use crate::schema::SchemaCache;

#[derive(Debug)]
pub struct StartError {
    pub violations: Vec<String>,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.violations.join("; "))
    }
}

impl std::error::Error for StartError {}

pub struct BusOptions {
    pub exe_dir: PathBuf,
    pub home: PathBuf,
    pub env: HashMap<String, String>,
    #[cfg(feature = "testing")]
    pub test_hooks: crate::testing::TestHooks,
}

impl BusOptions {
    #[must_use]
    pub fn new(exe_dir: PathBuf, home: PathBuf, env: HashMap<String, String>) -> Self {
        Self {
            exe_dir,
            home,
            env,
            #[cfg(feature = "testing")]
            test_hooks: crate::testing::TestHooks::default(),
        }
    }

    #[cfg(feature = "testing")]
    #[must_use]
    pub fn with_test_hooks(mut self, test_hooks: crate::testing::TestHooks) -> Self {
        self.test_hooks = test_hooks;
        self
    }
}

struct Command {
    method: String,
    params: Value,
    plugin: Option<String>,
    reply: oneshot::Sender<Result<Value, RpcError>>,
}

struct Inner {
    commands: mpsc::UnboundedSender<Command>,
    plugins: Mutex<Vec<PluginInfo>>,
}

pub struct CallBus {
    inner: Arc<Inner>,
}

impl CallBus {
    pub async fn new(options: BusOptions) -> Result<Arc<Self>, StartError> {
        let handle = Handle::try_current().map_err(|_| StartError {
            violations: vec!["CallBus::new requires a tokio runtime".into()],
        })?;
        let mut violations = Vec::new();
        let plugins_dir = options.exe_dir.join("plugins");
        let mut slots = match read_slots(&plugins_dir) {
            Ok(slots) => slots,
            Err(error) => {
                violations.push(error);
                Vec::new()
            },
        };
        if slots.is_empty() && violations.is_empty() {
            violations.push(format!("no plugins in {}", plugins_dir.display()));
        }
        let activation: Vec<String> = slots
            .iter()
            .filter(|slot| {
                slot.document.as_ref().is_some_and(|document| {
                    document
                        .provides
                        .iter()
                        .any(|provide| provide.point == "bus.activation")
                })
            })
            .map(|slot| slot.name.clone())
            .collect();
        if activation.len() != 1 {
            violations.push(format!(
                "bus.activation has {} providers: {}",
                activation.len(),
                activation.join(", ")
            ));
        }
        if !violations.is_empty() {
            return Err(StartError { violations });
        }
        let bootstrap_docs: Vec<PluginDocument> = slots
            .iter()
            .filter(|slot| slot.bootstrap)
            .filter_map(|slot| slot.document.clone())
            .collect();
        let infos: Vec<PluginInfo> = slots.iter().map(slot_info).collect();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            commands: tx,
            plugins: Mutex::new(infos),
        });
        let bus = Arc::new(Self {
            inner: Arc::clone(&inner),
        });
        let engine = match crate::wasm::engine() {
            Ok(engine) => engine,
            Err(error) => return Err(StartError { violations: vec![error] }),
        };
        let env_pairs: Vec<(String, String)> =
            options.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
        let (host_tx, mut host_rx) = mpsc::unbounded_channel();
        let bound_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        #[cfg(feature = "testing")]
        let test_hooks = options.test_hooks.clone();
        let mut loaded = match load_plugins(
            &engine,
            &plugins_dir,
            &bootstrap_docs,
            &env_pairs,
            host_tx.clone(),
            Arc::clone(&bound_paths),
            #[cfg(feature = "testing")]
            test_hooks.clone(),
        )
        .await
        {
            Ok(loaded) => loaded,
            Err(error) => return Err(StartError { violations: vec![error] }),
        };
        let activation_name = activation[0].clone();
        let activation_result = {
            let activation_link = loaded.get(&activation_name).ok_or_else(|| StartError {
                violations: vec![format!("bootstrap plugin {activation_name} did not start")],
            })?;
            call_link(
                activation_link,
                "bus.activation",
                Value::Object(serde_json::Map::new()),
            )
            .await
        }
        .map_err(|error| StartError {
            violations: vec![format!("bus.activation: {error}")],
        })?;
        let disabled_names = disabled_names_from(&activation_result).map_err(|error| StartError {
            violations: vec![error],
        })?;
        let mut enabled_docs = Vec::new();
        for slot in &mut slots {
            if slot.bootstrap {
                slot.status = PluginStatus::Enabled;
                continue;
            }
            if slot.document.is_none() {
                continue;
            }
            if disabled_names.iter().any(|name| name == &slot.name) {
                slot.status = PluginStatus::Disabled;
            } else if let Some(document) = slot.document.clone() {
                slot.status = PluginStatus::Enabled;
                enabled_docs.push(document);
            }
        }
        for document in enabled_docs {
            match load_plugins(
                &engine,
                &plugins_dir,
                &[document.clone()],
                &env_pairs,
                host_tx.clone(),
                Arc::clone(&bound_paths),
                #[cfg(feature = "testing")]
                test_hooks.clone(),
            )
            .await
            {
                Ok(mut started) => {
                    if let Some(link) = started.remove(&document.name) {
                        loaded.insert(document.name.clone(), link);
                    }
                },
                Err(error) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(plugin = %document.name, reason = %error, "plugin failed to start");
                    if let Some(slot) = slots.iter_mut().find(|slot| slot.name == document.name) {
                        slot.status = PluginStatus::Failed { reason: error };
                    }
                },
            }
        }
        let session = Arc::new(Session {
            engine,
            plugins_dir,
            env_pairs,
            host_tx,
            bound_paths,
            links: Mutex::new(loaded),
            slots: Mutex::new(slots),
            reload_gate: Mutex::new(()),
            #[cfg(feature = "testing")]
            test_hooks,
        });
        publish_infos(&session, &inner.plugins).await;
        let epoch_engine = session.engine.clone();
        let session_for_loop = Arc::clone(&session);
        handle.spawn(async move {
            let session = session_for_loop;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
                loop {
                    tick.tick().await;
                    epoch_engine.increment_epoch();
                }
            });
            loop {
                tokio::select! {
                    command = rx.recv() => {
                        let Some(command) = command else { break; };
                        if command.method == "bus.close" {
                            let names = session
                                .links
                                .lock()
                                .await
                                .keys()
                                .cloned()
                                .collect::<Vec<_>>();
                            for name in names {
                                stop_named(&session, &name).await;
                            }
                            let _ = command.reply.send(Ok(Value::Null));
                            break;
                        }
                        if command.method == "bus.plugins.list" {
                            let infos = inner.plugins.lock().await.clone();
                            let result = serde_json::to_value(infos).map_err(|error| {
                                RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string())
                            });
                            let _ = command.reply.send(result);
                            continue;
                        }
                        if command.method == "bus.plugins.reload" || command.method == "bus.plugins.rescan" {
                            let session = Arc::clone(&session);
                            let infos = Arc::clone(&inner);
                            tokio::spawn(async move {
                                let result = if command.method == "bus.plugins.reload" {
                                    reload(&session, &infos.plugins).await
                                } else {
                                    rescan(&session, &infos.plugins).await
                                };
                                let _ = command.reply.send(result);
                            });
                            continue;
                        }
                        let documents = session
                            .slots
                            .lock()
                            .await
                            .iter()
                            .filter_map(|slot| slot.document.clone())
                            .collect::<Vec<_>>();
                        let loaded = call_map(&session.links).await;
                        let plugin = command.plugin.clone();
                        let infos = Arc::clone(&inner);
                        let session = Arc::clone(&session);
                        tokio::spawn(async move {
                            let started = Instant::now();
                            let result = dispatch(
                                &documents,
                                &loaded,
                                &command.method,
                                command.params,
                                plugin.as_deref(),
                                &infos.plugins,
                                &session,
                            )
                            .await;
                            let code = result.as_ref().err().map(|error| error.code).unwrap_or(0);
                            metrics::record_call("call", &command.method, code, started.elapsed().as_secs_f64());
                            let _ = command.reply.send(result);
                        });
                    }
                    host = host_rx.recv() => {
                        let Some(host) = host else { break; };
                        let documents = session
                            .slots
                            .lock()
                            .await
                            .iter()
                            .filter_map(|slot| slot.document.clone())
                            .collect::<Vec<_>>();
                        let loaded = call_map(&session.links).await;
                        let infos = Arc::clone(&inner);
                        let session = Arc::clone(&session);
                        tokio::spawn(async move {
                            let response = handle_host(&documents, &loaded, host.kind, &infos.plugins, &session).await;
                            let _ = host.reply.send(response);
                        });
                    }
                }
            }
        });
        let _ = handle;
        Ok(bus)
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .commands
            .send(Command {
                method: method.to_owned(),
                params,
                plugin: None,
                reply: tx,
            })
            .map_err(|_| RpcError::new(jsonrpc::PROVIDER_UNAVAILABLE, "bus is closed"))?;
        rx.await
            .map_err(|_| RpcError::new(jsonrpc::PROVIDER_UNAVAILABLE, "bus dropped the call"))?
    }

    pub async fn call_as<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, RpcError> {
        let params = serde_json::to_value(params)
            .map_err(|error| RpcError::new(jsonrpc::INVALID_PARAMS, error.to_string()))?;
        let value = self.call(method, params).await?;
        serde_json::from_value(value)
            .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))
    }

    pub async fn providers(&self, point: &str) -> Vec<ProviderInfo> {
        let plugins = self.inner.plugins.lock().await;
        let mut found = Vec::new();
        for plugin in plugins.iter().filter(|plugin| plugin.status.is_enabled()) {
            let Some(raw) = plugin.document.as_ref() else {
                continue;
            };
            let Ok(document) = parse_document(raw) else {
                continue;
            };
            for provide in document.provides {
                if provide.point == point {
                    found.push(ProviderInfo {
                        plugin: plugin.name.clone(),
                        order: provide.order,
                        requires: provide.requires,
                        meta: provide.meta,
                    });
                }
            }
        }
        found.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.plugin.cmp(&right.plugin))
        });
        found
    }

    pub async fn invoke(&self, point: &str, plugin: &str, params: Value) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .commands
            .send(Command {
                method: point.to_owned(),
                params,
                plugin: Some(plugin.to_owned()),
                reply: tx,
            })
            .map_err(|_| RpcError::new(jsonrpc::PROVIDER_UNAVAILABLE, "bus is closed"))?;
        rx.await
            .map_err(|_| RpcError::new(jsonrpc::PROVIDER_UNAVAILABLE, "bus dropped the call"))?
    }

    pub async fn plugins(&self) -> Vec<PluginInfo> {
        self.inner.plugins.lock().await.clone()
    }

    pub async fn close(&self) {
        let (tx, rx) = oneshot::channel();
        let _ = self.inner.commands.send(Command {
            method: "bus.close".into(),
            params: Value::Null,
            plugin: None,
            reply: tx,
        });
        let _ = rx.await;
    }
}

struct Slot {
    name: String,
    document: Option<PluginDocument>,
    pending: Option<PluginDocument>,
    removed: bool,
    status: PluginStatus,
    bootstrap: bool,
}

fn slot_info(slot: &Slot) -> PluginInfo {
    PluginInfo {
        name: slot.name.clone(),
        api: slot.document.as_ref().map(|document| document.api.clone()),
        version: slot.document.as_ref().map(|document| document.version.clone()),
        status: slot.status.clone(),
        document: slot.document.as_ref().map(|document| document.raw.clone()),
    }
}

fn read_slots(dir: &Path) -> Result<Vec<Slot>, String> {
    let cache = SchemaCache::new()?;
    let mut slots = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        match read_slot_document(&cache, &entry.path(), &name) {
            Ok(document) => {
                let bootstrap = document
                    .provides
                    .iter()
                    .any(|provide| provide.point.starts_with("bus."));
                slots.push(Slot {
                    name,
                    document: Some(document),
                    pending: None,
                    removed: false,
                    status: PluginStatus::Disabled,
                    bootstrap,
                });
            },
            Err(reason) => {
                #[cfg(feature = "tracing")]
                tracing::error!(plugin = %name, reason = %reason, "plugin document failed");
                slots.push(Slot {
                    name,
                    document: None,
                    pending: None,
                    removed: false,
                    status: PluginStatus::Failed { reason },
                    bootstrap: false,
                });
            },
        }
    }
    slots.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(slots)
}

fn read_slot_document(cache: &SchemaCache, dir: &Path, name: &str) -> Result<PluginDocument, String> {
    let openrpc = dir.join("openrpc.json");
    let text = std::fs::read_to_string(&openrpc).map_err(|error| {
        format!("{}: {error}", openrpc.display())
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    cache
        .validate_document(&value)
        .map_err(|error| format!("{}: {error}", openrpc.display()))?;
    let document = parse_document(&value).map_err(|error| error.to_string())?;
    if name != document.name {
        return Err(format!(
            "plugin directory {name} does not match {}",
            document.name
        ));
    }
    Ok(document)
}

fn disabled_names_from(value: &Value) -> Result<Vec<String>, String> {
    let Some(items) = value.get("disabled").and_then(Value::as_array) else {
        return Err("bus.activation result has no disabled array".into());
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "bus.activation disabled entry is not a string".to_owned())
        })
        .collect()
}

async fn call_link(link: &PluginLink, method: &str, params: Value) -> Result<Value, String> {
    let request = jsonrpc::Request::new(1, method, params);
    let text = jsonrpc::encode(&request).map_err(|error| error.to_string())?;
    let (reply_tx, reply_rx) = oneshot::channel();
    link.calls
        .send(crate::wasm::PluginCall {
            method: method.to_owned(),
            request: text,
            reply: reply_tx,
        })
        .map_err(|error| error.to_string())?;
    let response = reply_rx
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error)?;
    let value: Value = serde_json::from_str(&response).map_err(|error| error.to_string())?;
    if let Some(error) = value.get("error") {
        return Err(error.to_string());
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| "plugin response has no result".to_owned())
}

struct Session {
    engine: wasmtime::Engine,
    plugins_dir: PathBuf,
    env_pairs: Vec<(String, String)>,
    host_tx: mpsc::UnboundedSender<crate::wasm::HostCommand>,
    bound_paths: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    links: Mutex<HashMap<String, PluginLink>>,
    slots: Mutex<Vec<Slot>>,
    reload_gate: Mutex<()>,
    #[cfg(feature = "testing")]
    test_hooks: crate::testing::TestHooks,
}

async fn load_one(session: &Session, document: &PluginDocument) -> Result<PluginLink, String> {
    let mut loaded = load_plugins(
        &session.engine,
        &session.plugins_dir,
        std::slice::from_ref(document),
        &session.env_pairs,
        session.host_tx.clone(),
        Arc::clone(&session.bound_paths),
        #[cfg(feature = "testing")]
        session.test_hooks.clone(),
    )
    .await?;
    loaded
        .remove(&document.name)
        .ok_or_else(|| format!("plugin {} did not start", document.name))
}

async fn mark_failed(session: &Session, infos: &Mutex<Vec<PluginInfo>>, name: &str, reason: &str) {
    {
        let mut slots = session.slots.lock().await;
        if let Some(slot) = slots.iter_mut().find(|slot| slot.name == name) {
            if let Some(pending) = slot.pending.take() {
                slot.document = Some(pending);
            }
            slot.status = PluginStatus::Failed {
                reason: reason.to_owned(),
            };
        }
    }
    stop_named(session, name).await;
    metrics::record_transition(name, "failed");
    publish_infos(session, infos).await;
}

async fn publish_infos(session: &Session, infos: &Mutex<Vec<PluginInfo>>) {
    let slots = session.slots.lock().await;
    *infos.lock().await = slots.iter().map(slot_info).collect();
}

async fn stop_named(session: &Session, name: &str) {
    let (stop, done) = {
        let links = session.links.lock().await;
        let Some(link) = links.get(name) else {
            return;
        };
        (link.stop.lock().await.take(), link.stopped.lock().await.take())
    };
    if let Some(stop) = stop {
        let _ = stop.send(());
    }
    if let Some(done) = done {
        let _ = done.await;
    }
    session.links.lock().await.remove(name);
}

async fn reload(session: &Session, infos: &Mutex<Vec<PluginInfo>>) -> Result<Value, RpcError> {
    let _gate = session.reload_gate.lock().await;
    let activation_name = {
        let slots = session.slots.lock().await;
        slots
            .iter()
            .find(|slot| {
                slot.bootstrap
                    && slot.document.as_ref().is_some_and(|document| {
                        document
                            .provides
                            .iter()
                            .any(|provide| provide.point == "bus.activation")
                    })
            })
            .map(|slot| slot.name.clone())
    };
    let Some(activation_name) = activation_name else {
        return Err(RpcError::new(
            jsonrpc::PROVIDER_UNAVAILABLE,
            "bus.activation has 0 providers",
        ));
    };
    let sender = session
        .links
        .lock()
        .await
        .get(&activation_name)
        .map(|link| link.calls.clone());
    let Some(sender) = sender else {
        return Err(RpcError::new(
            jsonrpc::PROVIDER_UNAVAILABLE,
            "bus.activation is not loaded",
        ));
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let request = jsonrpc::Request::new(1, "bus.activation", Value::Object(serde_json::Map::new()));
    let text = jsonrpc::encode(&request)?;
    sender
        .send(crate::wasm::PluginCall {
            method: "bus.activation".into(),
            request: text,
            reply: reply_tx,
        })
        .map_err(|_| RpcError::new(jsonrpc::PROVIDER_UNAVAILABLE, "bus.activation task is gone"))?;
    let response = reply_rx
        .await
        .map_err(|_| RpcError::new(jsonrpc::PROVIDER_UNAVAILABLE, "bus.activation dropped the call"))?
        .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error))?;
    let value: Value = serde_json::from_str(&response)
        .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))?;
    if let Some(error) = value.get("error") {
        #[cfg(feature = "tracing")]
        tracing::error!(error = %error, "bus.plugins.reload");
        let rpc = serde_json::from_value(error.clone())
            .unwrap_or_else(|_| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()));
        return Err(rpc);
    }
    let result = value
        .get("result")
        .cloned()
        .ok_or_else(|| RpcError::new(jsonrpc::INTERNAL_ERROR, "bus.activation has no result"))?;
    let disabled_names = disabled_names_from(&result)
        .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error))?;
    let (all, enabled, bootstrap) = {
        let slots = session.slots.lock().await;
        let all = slots
            .iter()
            .filter(|slot| slot.document.is_some() && !slot.removed)
            .map(|slot| slot.name.clone())
            .collect::<Vec<_>>();
        let enabled = slots
            .iter()
            .filter(|slot| matches!(slot.status, PluginStatus::Enabled))
            .map(|slot| slot.name.clone())
            .collect::<Vec<_>>();
        let bootstrap = slots
            .iter()
            .filter(|slot| slot.bootstrap)
            .map(|slot| slot.name.clone())
            .collect::<Vec<_>>();
        (all, enabled, bootstrap)
    };
    let diff = crate::activation::diff(&all, &enabled, &disabled_names, &bootstrap);
    for name in diff.disable {
        {
            let mut slots = session.slots.lock().await;
            if let Some(slot) = slots.iter_mut().find(|slot| slot.name == name) {
                slot.status = PluginStatus::Stopping;
            }
        }
        publish_infos(session, infos).await;
        stop_named(session, &name).await;
        {
            let mut slots = session.slots.lock().await;
            if let Some(slot) = slots.iter_mut().find(|slot| slot.name == name) {
                if let Some(pending) = slot.pending.take() {
                    slot.document = Some(pending);
                }
                if slot.removed {
                    slots.retain(|item| item.name != name);
                } else {
                    slot.status = PluginStatus::Disabled;
                }
            }
        }
        metrics::record_transition(&name, "disabled");
        publish_infos(session, infos).await;
    }
    for name in diff.enable {
        let document = {
            let slots = session.slots.lock().await;
            slots
                .iter()
                .find(|slot| slot.name == name)
                .and_then(|slot| slot.document.clone())
        };
        let Some(document) = document else {
            continue;
        };
        match load_one(session, &document).await {
            Ok(link) => {
                session.links.lock().await.insert(name.clone(), link);
                let mut slots = session.slots.lock().await;
                if let Some(slot) = slots.iter_mut().find(|slot| slot.name == name) {
                    slot.status = PluginStatus::Enabled;
                }
                metrics::record_transition(&name, "enabled");
            },
            Err(error) => {
                #[cfg(feature = "tracing")]
                tracing::error!(plugin = %name, reason = %error, "plugin failed to start");
                let mut slots = session.slots.lock().await;
                if let Some(slot) = slots.iter_mut().find(|slot| slot.name == name) {
                    slot.status = PluginStatus::Failed { reason: error };
                }
                metrics::record_transition(&name, "failed");
            },
        }
        publish_infos(session, infos).await;
    }
    publish_infos(session, infos).await;
    Ok(Value::Null)
}

async fn rescan(session: &Session, infos: &Mutex<Vec<PluginInfo>>) -> Result<Value, RpcError> {
    let cache = SchemaCache::new().map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error))?;
    let entries = std::fs::read_dir(&session.plugins_dir)
        .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))?;
    let mut seen = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        seen.push(name.clone());
        let parsed = read_slot_document(&cache, &entry.path(), &name);
        let mut slots = session.slots.lock().await;
        if let Some(slot) = slots.iter_mut().find(|slot| slot.name == name) {
            if slot.bootstrap {
                continue;
            }
            let document = match parsed {
                Ok(document) => document,
                Err(reason) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(plugin = %name, reason = %reason, "rescan skipped a document that did not parse");
                    #[cfg(not(feature = "tracing"))]
                    let _ = reason;
                    continue;
                },
            };
            if document
                .provides
                .iter()
                .any(|provide| provide.point.starts_with("bus."))
            {
                #[cfg(feature = "tracing")]
                tracing::error!(plugin = %name, "rescan ignored a bootstrap document");
                continue;
            }
            let running = matches!(slot.status, PluginStatus::Enabled | PluginStatus::Stopping);
            if running {
                slot.pending = Some(document);
            } else {
                slot.document = Some(document);
                slot.removed = false;
                if matches!(slot.status, PluginStatus::Failed { .. }) {
                    slot.status = PluginStatus::Disabled;
                    metrics::record_transition(&name, "disabled");
                }
            }
        } else {
            match parsed {
                Err(reason) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(plugin = %name, reason = %reason, "plugin document failed");
                    slots.push(Slot {
                        name: name.clone(),
                        document: None,
                        pending: None,
                        removed: false,
                        status: PluginStatus::Failed { reason },
                        bootstrap: false,
                    });
                    metrics::record_transition(&name, "failed");
                },
                Ok(document)
                    if document
                        .provides
                        .iter()
                        .any(|provide| provide.point.starts_with("bus.")) =>
                {
                    #[cfg(feature = "tracing")]
                    tracing::error!(plugin = %name, "rescan ignored a new bootstrap plugin");
                },
                Ok(document) => {
                    slots.push(Slot {
                        name: name.clone(),
                        document: Some(document),
                        pending: None,
                        removed: false,
                        status: PluginStatus::Disabled,
                        bootstrap: false,
                    });
                },
            }
        }
    }
    {
        let mut slots = session.slots.lock().await;
        for slot in slots.iter_mut() {
            if slot.bootstrap || seen.iter().any(|name| name == &slot.name) {
                continue;
            }
            slot.removed = true;
        }
    }
    publish_infos(session, infos).await;
    Ok(Value::Null)
}

async fn load_plugins(
    engine: &wasmtime::Engine,
    plugins_dir: &Path,
    documents: &[PluginDocument],
    env: &[(String, String)],
    host_tx: mpsc::UnboundedSender<crate::wasm::HostCommand>,
    bound_paths: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    #[cfg(feature = "testing")] test_hooks: crate::testing::TestHooks,
) -> Result<HashMap<String, PluginLink>, String> {
    let linker = crate::wasm::linker(engine)?;
    let mut loaded = HashMap::new();
    for document in documents {
        let wasm = plugins_dir.join(&document.name).join("plugin.wasm");
        let component = crate::wasm::load_component(engine, wasm).await?;
        let store = wasmtime::Store::new(
            engine,
            crate::wasm::PluginHost {
                name: document.name.clone(),
                ctx: crate::wasm::wasi_ctx(env)?,
                table: wasmtime::component::ResourceTable::new(),
                host_tx: host_tx.clone(),
                catalog: Arc::new(documents.to_vec()),
                declarations: document.resources.clone(),
                resources: std::sync::Mutex::new(crate::resources::ResourceSet::default()),
                bound_paths: Arc::clone(&bound_paths),
                #[cfg(feature = "testing")]
                test_hooks: test_hooks.clone(),
            },
        );
        let plugin = crate::wasm::spawn_plugin(engine.clone(), component, store, &linker).await?;
        plugin
            .started
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("plugin {} start: {error}", document.name))?;
        loaded.insert(
            document.name.clone(),
            PluginLink {
                calls: plugin.calls,
                stop: Mutex::new(Some(plugin.stop)),
                stopped: Mutex::new(Some(plugin.stopped)),
            },
        );
    }
    Ok(loaded)
}

struct PluginLink {
    calls: tokio::sync::mpsc::UnboundedSender<crate::wasm::PluginCall>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    stopped: Mutex<Option<oneshot::Receiver<()>>>,
}

type CallMap = HashMap<String, mpsc::UnboundedSender<crate::wasm::PluginCall>>;

async fn call_map(links: &Mutex<HashMap<String, PluginLink>>) -> CallMap {
    links
        .lock()
        .await
        .iter()
        .map(|(name, link)| (name.clone(), link.calls.clone()))
        .collect()
}

async fn handle_host(
    documents: &[PluginDocument],
    loaded: &CallMap,
    kind: crate::wasm::HostKind,
    infos: &Mutex<Vec<PluginInfo>>,
    session: &Session,
) -> String {
    use crate::wasm::HostKind;
    match kind {
        HostKind::Call { request } | HostKind::Invoke { request, .. } => {
            let parsed = match jsonrpc::parse_request(&request) {
                Ok(parsed) => parsed,
                Err(error) => return jsonrpc::encode(&jsonrpc::Response::err(None, error)).unwrap_or_default(),
            };
            let id = parsed.id.clone();
            let result = if parsed.method == "bus.plugins.reload" {
                reload(session, infos).await
            } else if parsed.method == "bus.plugins.rescan" {
                rescan(session, infos).await
            } else {
                dispatch(
                    documents,
                    loaded,
                    &parsed.method,
                    parsed.params,
                    None,
                    infos,
                    session,
                )
                .await
            };
            let response = match result {
                Ok(value) => jsonrpc::Response::ok(id, value),
                Err(error) => jsonrpc::Response::err(id, error),
            };
            jsonrpc::encode(&response).unwrap_or_default()
        },
        HostKind::Publish { notification, .. } => {
            let value: Value = serde_json::from_str(&notification).unwrap_or(Value::Null);
            let method = value.get("method").and_then(Value::as_str).unwrap_or("");
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let mut delivered = 0u32;
            for document in documents {
                if !document.subscribes.iter().any(|item| item == method) {
                    continue;
                }
                let Some(tx) = loaded.get(&document.name) else { continue; };
                let request = jsonrpc::Request::new(delivered as u64 + 1, method, params.clone());
                let Ok(text) = jsonrpc::encode(&request) else { continue; };
                let (reply_tx, reply_rx) = oneshot::channel();
                if tx.send(crate::wasm::PluginCall { method: method.to_owned(), request: text, reply: reply_tx }).is_err() {
                    continue;
                }
                let _ = reply_rx.await;
                delivered = delivered.saturating_add(1);
            }
            delivered.to_string()
        },
    }
}

async fn dispatch(
    documents: &[PluginDocument],
    loaded: &CallMap,
    method: &str,
    params: Value,
    plugin: Option<&str>,
    infos: &Mutex<Vec<PluginInfo>>,
    session: &Session,
) -> Result<Value, RpcError> {
    if method == "rpc.discover" {
        let infos = infos.lock().await;
        let docs = infos
            .iter()
            .filter(|info| info.status.is_enabled())
            .filter_map(|info| info.document.clone())
            .collect::<Vec<_>>();
        return Ok(Value::Array(docs));
    }
    if method == "bus.plugins.list" {
        let infos = infos.lock().await.clone();
        return serde_json::to_value(infos)
            .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()));
    }
    if method == "bus.close" || method == "rpc.discover" {
        return route(documents, method, params).await;
    }
    let owner = documents.iter().find(|document| {
        if let Some(plugin) = plugin {
            if document.name != plugin {
                return false;
            }
        }
        document.methods.iter().any(|item| item.address == method)
            || document.provides.iter().any(|item| item.point == method)
    });
    let Some(owner) = owner else {
        return route(documents, method, params).await;
    };
    let status = infos
        .lock()
        .await
        .iter()
        .find(|info| info.name == owner.name)
        .map(|info| info.status.clone());
    if !matches!(status, Some(PluginStatus::Enabled)) {
        let reason = match status {
            Some(PluginStatus::Failed { reason }) => reason,
            Some(other) => other.as_label().to_owned(),
            None => "not loaded".to_owned(),
        };
        return Err(RpcError::new(
            jsonrpc::PROVIDER_UNAVAILABLE,
            format!("{method} provider {} is {reason}", owner.name),
        )
        .with_data(serde_json::json!({ "reason": reason })));
    }
    let Some(tx) = loaded.get(&owner.name) else {
        return Err(RpcError::new(
            jsonrpc::PROVIDER_UNAVAILABLE,
            format!("{method} provider {} is not loaded", owner.name),
        ));
    };
    if let Some(declared) = owner.methods.iter().find(|item| item.address == method) {
        if let Err(error) = SchemaCache::check_in_document(
            &owner.name,
            &owner.raw,
            &declared.params,
            &params,
        ) {
            return Err(RpcError::new(jsonrpc::INVALID_PARAMS, error));
        }
    }
    let request = jsonrpc::Request::new(1, method, params);
    let text = jsonrpc::encode(&request)?;
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(crate::wasm::PluginCall {
        method: method.to_owned(),
        request: text,
        reply: reply_tx,
    })
    .map_err(|_| RpcError::new(jsonrpc::PROVIDER_UNAVAILABLE, "plugin task is gone"))?;
    let response = match reply_rx.await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            #[cfg(feature = "tracing")]
            tracing::error!(address = method, error = %error, "plugin trap");
            mark_failed(session, infos, &owner.name, &error).await;
            return Err(RpcError::new(jsonrpc::INTERNAL_ERROR, error));
        },
        Err(_) => {
            return Err(RpcError::new(
                jsonrpc::PROVIDER_UNAVAILABLE,
                "plugin dropped the call",
            ));
        },
    };
    let value: Value = serde_json::from_str(&response)
        .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))?;
    if let Some(error) = value.get("error") {
        let rpc = serde_json::from_value::<RpcError>(error.clone())
            .unwrap_or_else(|_| RpcError::new(jsonrpc::INTERNAL_ERROR, response));
        if let Some(declared) = owner.methods.iter().find(|item| item.address == method) {
            if rpc.code > 0 && !declared.errors.contains(&rpc.code) {
                #[cfg(feature = "tracing")]
                tracing::error!(address = method, code = rpc.code, "handler error code is not declared");
            }
        }
        return Err(rpc);
    }
    let result = value
        .get("result")
        .cloned()
        .ok_or_else(|| RpcError::new(jsonrpc::INTERNAL_ERROR, "plugin response has no result"))?;
    if let Some(declared) = owner.methods.iter().find(|item| item.address == method) {
        if let Err(error) =
            SchemaCache::check_in_document(&owner.name, &owner.raw, &declared.result, &result)
        {
            #[cfg(feature = "tracing")]
            tracing::error!(address = method, error = %error, "result schema");
            return Err(RpcError::new(jsonrpc::INTERNAL_ERROR, error));
        }
    }
    Ok(result)
}

async fn route(
    documents: &[PluginDocument],
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    if method == "bus.close" {
        return Ok(Value::Null);
    }
    if method == "rpc.discover" {
        return Ok(Value::Array(
            documents.iter().map(|document| document.raw.clone()).collect(),
        ));
    }
    if method == "bus.plugins.list" {
        let infos: Vec<_> = documents
            .iter()
            .map(|document| PluginInfo {
                name: document.name.clone(),
                api: Some(document.api.clone()),
                version: Some(document.version.clone()),
                status: PluginStatus::Enabled,
                document: Some(document.raw.clone()),
            })
            .collect();
        return serde_json::to_value(infos)
            .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()));
    }
    if documents.iter().all(|document| {
        document
            .methods
            .iter()
            .all(|item| item.address != method)
            && document
                .points
                .iter()
                .all(|point| format!("{}.{}", document.api, point.name) != method)
    }) && !method.starts_with("bus.")
    {
        return Err(RpcError::new(
            jsonrpc::METHOD_NOT_FOUND,
            format!("method {method} is not declared"),
        ));
    }
    Err(RpcError::new(
        jsonrpc::PROVIDER_UNAVAILABLE,
        format!("{method} is not loaded"),
    ))
}
