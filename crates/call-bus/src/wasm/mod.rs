//! WASM component loader. One store task per plugin.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{mpsc, oneshot};
use wasmtime::component::{Component, Linker, ResourceTable, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

use crate::document::{Access, ResourceDecl, ResourceKind};
use crate::resources::{self, ResourceSet};

pub struct LoadedPlugin {
    pub calls: mpsc::UnboundedSender<PluginCall>,
    pub stop: oneshot::Sender<()>,
    pub started: oneshot::Receiver<Result<(), String>>,
    pub stopped: oneshot::Receiver<()>,
}

pub struct PluginCall {
    pub method: String,
    pub request: String,
    pub reply: oneshot::Sender<Result<String, String>>,
}

pub struct HostCommand {
    pub plugin: String,
    pub kind: HostKind,
    pub reply: oneshot::Sender<String>,
}

pub enum HostKind {
    Call { request: String },
    Publish { notification: String, delivery: String },
    Invoke { point: String, plugin: String, request: String },
}

#[derive(wasmtime::component::ComponentType, wasmtime::component::Lift)]
#[component(enum)]
#[repr(u8)]
enum Delivery {
    #[component(name = "wait")]
    Wait,
    #[component(name = "queue")]
    Queue,
    #[component(name = "detach")]
    Detach,
}

#[derive(wasmtime::component::ComponentType, wasmtime::component::Lift)]
#[component(enum)]
#[repr(u8)]
enum Level {
    #[component(name = "error")]
    Error,
    #[component(name = "warn")]
    Warn,
    #[component(name = "info")]
    Info,
    #[component(name = "debug")]
    Debug,
    #[component(name = "trace")]
    Trace,
}

pub struct PluginHost {
    pub name: String,
    pub ctx: WasiCtx,
    pub table: ResourceTable,
    pub host_tx: mpsc::UnboundedSender<HostCommand>,
    pub catalog: Arc<Vec<crate::document::PluginDocument>>,
    pub declarations: Vec<ResourceDecl>,
    pub resources: Mutex<ResourceSet>,
    pub bound_paths: Arc<Mutex<Vec<PathBuf>>>,
    #[cfg(feature = "testing")]
    pub test_hooks: crate::testing::TestHooks,
}

impl WasiView for PluginHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

pub fn engine() -> Result<Engine, String> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    config.concurrency_support(true);
    config.epoch_interruption(true);
    Engine::new(&config).map_err(|error| error.to_string())
}

pub async fn load_component(engine: &Engine, path: PathBuf) -> Result<Component, String> {
    let engine = engine.clone();
    tokio::task::spawn_blocking(move || Component::from_file(&engine, path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

pub fn wasi_ctx(env: &[(String, String)]) -> Result<WasiCtx, String> {
    let mut builder = WasiCtx::builder();
    let pairs: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    builder.envs(&pairs);
    Ok(builder.build())
}

fn host_sender(accessor: &wasmtime::component::Accessor<PluginHost>) -> (String, mpsc::UnboundedSender<HostCommand>) {
    accessor.with(|mut access| {
        let host = access.get();
        (host.name.clone(), host.host_tx.clone())
    })
}

async fn ask(accessor: &wasmtime::component::Accessor<PluginHost>, kind: HostKind) -> Result<String, wasmtime::Error> {
    let (plugin, tx) = host_sender(accessor);
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(HostCommand {
        plugin,
        kind,
        reply: reply_tx,
    })
    .map_err(|error| wasmtime::Error::msg(error.to_string()))?;
    reply_rx
        .await
        .map_err(|error| wasmtime::Error::msg(error.to_string()))
}

pub fn linker(engine: &Engine) -> Result<Linker<PluginHost>, String> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|error| error.to_string())?;
    let mut host = linker
        .instance("chelix:plugin/host@1.0.0")
        .map_err(|error| error.to_string())?;
    host.func_wrap_concurrent("call", |accessor, (request,): (String,)| {
        Box::pin(async move {
            let response = ask(&accessor, HostKind::Call { request }).await?;
            Ok((response,))
        })
    })
    .map_err(|error| error.to_string())?;
    host.func_wrap_concurrent(
        "publish",
        |accessor, (notification, delivery): (String, Delivery)| {
            Box::pin(async move {
                let delivery = match delivery {
                    Delivery::Wait => "wait",
                    Delivery::Queue => "queue",
                    Delivery::Detach => "detach",
                };
                let response = ask(
                    &accessor,
                    HostKind::Publish {
                        notification,
                        delivery: delivery.to_owned(),
                    },
                )
                .await?;
                let count: u32 = response
                    .parse()
                    .map_err(|error| wasmtime::Error::msg(format!("{error}")))?;
                Ok((Ok::<u32, String>(count),))
            })
        },
    )
    .map_err(|error| error.to_string())?;
    host.func_wrap_concurrent("invoke", |accessor, (point, plugin, request): (String, String, String)| {
        Box::pin(async move {
            let response = ask(
                &accessor,
                HostKind::Invoke {
                    point,
                    plugin,
                    request,
                },
            )
            .await?;
            Ok((response,))
        })
    })
    .map_err(|error| error.to_string())?;
    host.func_wrap(
        "providers",
        |store: wasmtime::StoreContextMut<'_, PluginHost>, (point,): (String,)| {
            let mut found = Vec::new();
            for document in &store.data().catalog {
                for provide in &document.provides {
                    if provide.point == point {
                        found.push(crate::info::ProviderInfo {
                            plugin: document.name.clone(),
                            order: provide.order,
                            requires: provide.requires.clone(),
                            meta: provide.meta.clone(),
                        });
                    }
                }
            }
            found.sort_by(|left, right| {
                left.order
                    .cmp(&right.order)
                    .then_with(|| left.plugin.cmp(&right.plugin))
            });
            let response = serde_json::to_string(&found).unwrap_or_else(|_| "[]".to_owned());
            Ok((response,))
        },
    )
    .map_err(|error| error.to_string())?;
    host.func_wrap_concurrent("sleep", |accessor, (duration_ms,): (u64,)| {
        Box::pin(async move {
            #[cfg(feature = "testing")]
            {
                let hooks = accessor.with(|mut access| access.get().test_hooks.clone());
                crate::testing::sleep_ms(&hooks, duration_ms).await;
            }
            #[cfg(not(feature = "testing"))]
            {
                let _ = &accessor;
                tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
            }
            Ok(())
        })
    })
    .map_err(|error| error.to_string())?;
    host.func_wrap(
        "log",
        |store: wasmtime::StoreContextMut<'_, PluginHost>, (level, message): (Level, String)| {
            let plugin = store.data().name.clone();
            #[cfg(feature = "tracing")]
            match level {
                Level::Error => tracing::error!(plugin = %plugin, level = "error", "{message}"),
                Level::Warn => tracing::warn!(plugin = %plugin, level = "warn", "{message}"),
                Level::Info => tracing::info!(plugin = %plugin, level = "info", "{message}"),
                Level::Debug => tracing::debug!(plugin = %plugin, level = "debug", "{message}"),
                Level::Trace => tracing::trace!(plugin = %plugin, level = "trace", "{message}"),
            }
            #[cfg(not(feature = "tracing"))]
            {
                let _ = (plugin, level, message);
            }
            Ok(())
        },
    )
    .map_err(|error| error.to_string())?;
    host.func_wrap_concurrent("bind", |accessor, (resource, path): (String, String)| {
        Box::pin(async move {
            let outcome = accessor.with(|mut access| bind_resource(access.get(), &resource, &path));
            Ok((outcome,))
        })
    })
    .map_err(|error| error.to_string())?;
    host.func_wrap_concurrent("read", |accessor, (resource, path): (String, String)| {
        Box::pin(async move {
            let located = accessor.with(|mut access| locate_file(access.get(), &resource, &path));
            let outcome = match located {
                Ok(file) => tokio::task::spawn_blocking(move || std::fs::read(file))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| error.to_string())),
                Err(error) => Err(error),
            };
            Ok((outcome,))
        })
    })
    .map_err(|error| error.to_string())?;
    host.func_wrap_concurrent(
        "atomic-write",
        |accessor, (resource, path, contents): (String, String, Vec<u8>)| {
            Box::pin(async move {
                #[cfg(feature = "testing")]
                if let Some(gate) = accessor.with(|mut access| access.get().test_hooks.write_gate.clone()) {
                    gate.wait().await;
                }
                let located = accessor.with(|mut access| {
                    let host = access.get();
                    let mode = host
                        .resources
                        .lock()
                        .map_err(|error| error.to_string())
                        .and_then(|set| {
                            let bound = set.get(&resource)?;
                            if bound.access != crate::document::Access::ReadWrite {
                                return Err(format!("resource {resource} is not read_write"));
                            }
                            resources::mode_bits(bound.file_mode)
                        })?;
                    locate_file(host, &resource, &path).map(|file| (file, mode))
                });
                let outcome = match located {
                    Ok((file, mode)) => atomic_write(file, contents, mode).await,
                    Err(error) => Err(error),
                };
                Ok((outcome,))
            })
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(linker)
}

fn bind_resource(host: &PluginHost, id: &str, path: &str) -> Result<(), String> {
    let decl = host
        .declarations
        .iter()
        .find(|decl| decl.id == id)
        .cloned()
        .ok_or_else(|| format!("resource {id} is not declared"))?;
    if decl.from.is_some() {
        return Err(format!("resource {id} is already bound from a root"));
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(format!("bind path {} is not absolute", path.display()));
    }
    if decl.create {
        let parent = match decl.kind {
            ResourceKind::Dir => path.clone(),
            ResourceKind::File => path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("resource {id} has no parent"))?,
            ResourceKind::Net { .. } => {
                return Err(format!("resource {id} is not a filesystem resource"));
            },
        };
        std::fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
    }
    let mut global = host
        .bound_paths
        .lock()
        .map_err(|error| error.to_string())?;
    if global.iter().any(|existing| resources::paths_intersect(existing, &path)) {
        return Err(format!("resource {id} intersects another plugin resource"));
    }
    let mut set = host.resources.lock().map_err(|error| error.to_string())?;
    set.bind(&decl, path.clone())?;
    global.push(path);
    Ok(())
}

fn locate_file(host: &PluginHost, id: &str, path: &str) -> Result<PathBuf, String> {
    let set = host.resources.lock().map_err(|error| error.to_string())?;
    let bound = set.get(id)?.clone();
    let relative = resources::relative_file(path)?;
    match bound.kind {
        ResourceKind::File => {
            if !relative.as_os_str().is_empty() {
                return Err(format!("file resource {id} requires an empty path"));
            }
            Ok(bound.path)
        },
        ResourceKind::Dir => Ok(bound.path.join(relative)),
        ResourceKind::Net { .. } => Err(format!("resource {id} is not a file")),
    }
}

async fn atomic_write(path: PathBuf, contents: Vec<u8>, mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let temp = parent.join(format!(".{name}.tmp"));
    let temp_for_write = temp.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::write(&temp_for_write, &contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp_for_write, std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        std::fs::rename(&temp_for_write, &path)?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

pub async fn spawn_plugin(
    _engine: Engine,
    component: Component,
    mut store: Store<PluginHost>,
    linker: &Linker<PluginHost>,
) -> Result<LoadedPlugin, String> {
    let instance = linker
        .instantiate_async(&mut store, &component)
        .await
        .map_err(|error| error.to_string())?;
    let guest = instance
        .get_export_index(&mut store, None, "chelix:plugin/guest@1.0.0")
        .or_else(|| instance.get_export_index(&mut store, None, "chelix:plugin/guest"))
        .ok_or_else(|| "guest export missing".to_owned())?;
    let handle_index = instance
        .get_export_index(&mut store, Some(&guest), "handle")
        .ok_or_else(|| "handle export missing".to_owned())?;
    let start_index = instance
        .get_export_index(&mut store, Some(&guest), "start")
        .ok_or_else(|| "start export missing".to_owned())?;
    let stop_index = instance.get_export_index(&mut store, Some(&guest), "stop");
    let handle = instance
        .get_func(&mut store, &handle_index)
        .ok_or_else(|| "handle is not a function".to_owned())?;
    let start = instance
        .get_func(&mut store, &start_index)
        .ok_or_else(|| "start is not a function".to_owned())?;
    let stop_func = stop_index.and_then(|index| instance.get_func(&mut store, &index));
    let (call_tx, mut call_rx) = mpsc::unbounded_channel();
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (stopped_tx, stopped_rx) = oneshot::channel();
    store.epoch_deadline_async_yield_and_update(1);
    tokio::spawn(async move {
        let result = store
            .run_concurrent(async move |accessor| {
                let mut start_results = vec![Val::Bool(false)];
                let started = match start.call_concurrent(accessor, &[], &mut start_results).await {
                    Ok(()) => Ok(()),
                    Err(error) => Err(error.to_string()),
                };
                let _ = ready_tx.send(started);
                let mut inflight = FuturesUnordered::new();
                let mut stopping = false;
                loop {
                    tokio::select! {
                        _ = &mut stop_rx, if !stopping => {
                            stopping = true;
                        }
                        Some(call) = call_rx.recv(), if !stopping => {
                            inflight.push(async {
                                let PluginCall { request, reply, .. } = call;
                                let params = vec![Val::String(request)];
                                let mut results = vec![Val::Bool(false)];
                                let response = match handle.call_concurrent(accessor, &params, &mut results).await {
                                    Ok(()) => match results.pop() {
                                        Some(Val::String(text)) => Ok(text),
                                        Some(other) => Err(format!("handle returned {other:?}")),
                                        None => Err("handle returned no value".into()),
                                    },
                                    Err(error) => Err(error.to_string()),
                                };
                                let _ = reply.send(response);
                            });
                        }
                        Some(()) = inflight.next(), if !inflight.is_empty() => {}
                    }
                    if stopping && inflight.is_empty() {
                        if let Some(stop_func) = stop_func {
                            let mut results = Vec::new();
                            let _ = stop_func.call_concurrent(accessor, &[], &mut results).await;
                        }
                        break;
                    }
                }
                Ok::<(), wasmtime::Error>(())
            })
            .await;
        if let Err(error) = result {
            #[cfg(feature = "tracing")]
            tracing::error!(error = %error, "plugin task stopped");
        }
        let _ = stopped_tx.send(());
    });
    Ok(LoadedPlugin {
        calls: call_tx,
        stop: stop_tx,
        started: ready_rx,
        stopped: stopped_rx,
    })
}
