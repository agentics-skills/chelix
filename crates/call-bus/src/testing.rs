//! Test seams for clocks, write gates, and plugin directories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::bus::{BusOptions, CallBus};

use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct TestHooks {
    pub clock: Option<Arc<TestClock>>,
    pub write_gate: Option<Arc<TestGate>>,
}

#[derive(Default)]
pub struct TestClock {
    now_ms: Mutex<u64>,
    notify: Notify,
}

impl TestClock {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn advance(&self, duration_ms: u64) {
        let mut now = self.now_ms.lock().unwrap_or_else(|error| error.into_inner());
        *now = now.saturating_add(duration_ms);
        drop(now);
        self.notify.notify_waiters();
    }

    pub async fn wait_until(&self, deadline_ms: u64) {
        loop {
            let now = *self.now_ms.lock().unwrap_or_else(|error| error.into_inner());
            if now >= deadline_ms {
                return;
            }
            self.notify.notified().await;
        }
    }
}

#[derive(Default)]
pub struct TestGate {
    open: Mutex<bool>,
    notify: Notify,
}

impl TestGate {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn open(&self) {
        *self.open.lock().unwrap_or_else(|error| error.into_inner()) = true;
        self.notify.notify_waiters();
    }

    pub async fn wait(&self) {
        loop {
            if *self.open.lock().unwrap_or_else(|error| error.into_inner()) {
                return;
            }
            self.notify.notified().await;
        }
    }
}

#[must_use]
pub fn built_plugins() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|error| {
        panic!("current_exe: {error}");
    });
    let mut dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if dir.file_name().and_then(|name| name.to_str()) == Some("deps") {
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        }
    }
    dir.join("plugins")
}

pub fn exe_dir(plugins: &[&str]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let dest = root.path().join("plugins");
    std::fs::create_dir_all(&dest).unwrap_or_else(|error| panic!("plugins dir: {error}"));
    let built = built_plugins();
    for name in plugins {
        let from = built.join(name);
        if !from.is_dir() {
            panic!("plugin {name} is missing from {}; run just build-plugins", from.display());
        }
        copy_dir(&from, &dest.join(name));
    }
    root
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|error| panic!("mkdir {}: {error}", to.display()));
    let entries = std::fs::read_dir(from).unwrap_or_else(|error| panic!("read {}: {error}", from.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("dir entry: {error}"));
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target)
                .unwrap_or_else(|error| panic!("copy {}: {error}", entry.path().display()));
        }
    }
}

pub fn shared_config_bus() -> Arc<CallBus> {
    static BUS: OnceLock<Arc<CallBus>> = OnceLock::new();
    BUS.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|error| panic!("runtime: {error}"));
            let root = exe_dir(&["config"]);
            let config_dir = root.path().join("config");
            std::fs::create_dir_all(&config_dir)
                .unwrap_or_else(|error| panic!("config dir: {error}"));
            std::fs::write(
                config_dir.join("chelix.toml"),
                "[tools]\nmax_tool_result_bytes = 50000\n[tools.execute_command]\nterminal_size = \"115x58\"\n",
            )
            .unwrap_or_else(|error| panic!("config file: {error}"));
            let mut env = HashMap::new();
            env.insert(
                "CHELIX_CONFIG_DIR".to_owned(),
                config_dir.to_string_lossy().to_string(),
            );
            env.insert("HOME".to_owned(), "/tmp".to_owned());
            let bus = runtime
                .block_on(CallBus::new(BusOptions::new(
                    root.path().to_path_buf(),
                    PathBuf::from("/tmp"),
                    env,
                )))
                .unwrap_or_else(|error| panic!("call bus: {error}"));
            std::mem::forget(root);
            let _ = tx.send(bus);
            runtime.block_on(std::future::pending::<()>());
        });
        rx.recv().unwrap_or_else(|error| panic!("call bus thread: {error}"))
    })
    .clone()
}

pub async fn sleep_ms(hooks: &TestHooks, duration_ms: u64) {
    if let Some(clock) = &hooks.clock {
        let now = *clock.now_ms.lock().unwrap_or_else(|error| error.into_inner());
        clock.wait_until(now.saturating_add(duration_ms)).await;
        return;
    }
    tokio::time::sleep(Duration::from_millis(duration_ms)).await;
}
