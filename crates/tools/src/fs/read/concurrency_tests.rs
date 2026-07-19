use {
    super::ReadTool,
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde_json::json,
    std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    },
    tokio::sync::{Notify, Semaphore, mpsc, oneshot},
};

use crate::{
    Result,
    command::{CommandOptions, CommandOutput},
    error::Error,
    fs::{
        EditTool, WriteTool,
        shared::{host_fs_operation_lock_key, with_fs_operation_lock},
    },
    sandbox::{Sandbox, SandboxConfig, SandboxId, SandboxMode, SandboxRouter},
};

use crate::sandbox::file_system::SandboxReadResult;

const EVENT_TIMEOUT: Duration = Duration::from_secs(2);
const NO_ENTRY_WINDOW: Duration = Duration::from_millis(100);

struct ActiveRead<'a> {
    counter: &'a AtomicUsize,
}

impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

struct CoordinatedSandbox {
    files: Mutex<HashMap<String, Vec<u8>>>,
    preparation_open: AtomicBool,
    preparation_calls: AtomicUsize,
    preparation_changed: Notify,
    preparation_gate: Notify,
    read_started_tx: mpsc::UnboundedSender<String>,
    read_release: Semaphore,
    active_reads: AtomicUsize,
    max_active_reads: AtomicUsize,
}

impl CoordinatedSandbox {
    fn new(
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (read_started_tx, read_started_rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                files: Mutex::new(files.into_iter().collect()),
                preparation_open: AtomicBool::new(false),
                preparation_calls: AtomicUsize::new(0),
                preparation_changed: Notify::new(),
                preparation_gate: Notify::new(),
                read_started_tx,
                read_release: Semaphore::new(0),
                active_reads: AtomicUsize::new(0),
                max_active_reads: AtomicUsize::new(0),
            }),
            read_started_rx,
        )
    }

    async fn wait_for_preparation_calls(&self, expected: usize) {
        loop {
            let changed = self.preparation_changed.notified();
            if self.preparation_calls.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::time::timeout(EVENT_TIMEOUT, changed)
                .await
                .expect("sandbox preparation did not reach expected call count");
        }
    }

    fn open_preparation(&self) {
        self.preparation_open.store(true, Ordering::SeqCst);
        self.preparation_gate.notify_waiters();
    }

    fn release_reads(&self, count: usize) {
        self.read_release.add_permits(count);
    }

    fn max_active_reads(&self) -> usize {
        self.max_active_reads.load(Ordering::SeqCst)
    }

    fn file_content(&self, path: &str) -> Vec<u8> {
        self.files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(path)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Sandbox for CoordinatedSandbox {
    fn backend_id(&self) -> crate::sandbox::SandboxBackendId {
        crate::sandbox::SandboxBackendId::Docker
    }

    fn provides_fs_isolation(&self) -> bool {
        true
    }

    async fn ensure_ready(&self, _id: &SandboxId) -> Result<()> {
        self.preparation_calls.fetch_add(1, Ordering::SeqCst);
        self.preparation_changed.notify_waiters();

        while !self.preparation_open.load(Ordering::SeqCst) {
            let opened = self.preparation_gate.notified();
            if self.preparation_open.load(Ordering::SeqCst) {
                break;
            }
            opened.await;
        }
        Ok(())
    }

    async fn run_command(
        &self,
        _id: &SandboxId,
        _command: &str,
        _opts: &CommandOptions,
    ) -> Result<CommandOutput> {
        Err(Error::message(
            "coordinated sandbox does not support command execution",
        ))
    }

    async fn read_file(
        &self,
        _id: &SandboxId,
        file_path: &str,
        max_bytes: u64,
    ) -> Result<SandboxReadResult> {
        let active = self.active_reads.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active_reads.fetch_max(active, Ordering::SeqCst);
        let _active_read = ActiveRead {
            counter: &self.active_reads,
        };

        self.read_started_tx
            .send(file_path.to_string())
            .map_err(|_| Error::message("read observer was dropped"))?;
        self.read_release
            .acquire()
            .await
            .map_err(|_| Error::message("read release gate was closed"))?
            .forget();

        let files = self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(content) = files.get(file_path) else {
            return Ok(SandboxReadResult::NotFound);
        };
        if content.len() as u64 > max_bytes {
            return Ok(SandboxReadResult::TooLarge(content.len() as u64));
        }
        Ok(SandboxReadResult::Ok(content.clone()))
    }

    async fn write_file(
        &self,
        _id: &SandboxId,
        file_path: &str,
        content: &[u8],
    ) -> Result<Option<serde_json::Value>> {
        self.files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(file_path.to_string(), content.to_vec());
        Ok(None)
    }

    async fn cleanup(&self, _id: &SandboxId) -> Result<()> {
        Ok(())
    }
}

fn sandbox_router(backend: Arc<CoordinatedSandbox>) -> Arc<SandboxRouter> {
    let backend: Arc<dyn Sandbox> = backend;
    Arc::new(SandboxRouter::with_backend(
        SandboxConfig {
            mode: SandboxMode::On,
            ..Default::default()
        },
        backend,
    ))
}

async fn next_read_started(receiver: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(EVENT_TIMEOUT, receiver.recv())
        .await
        .expect("sandbox read did not start")
        .expect("sandbox read observer closed")
}

async fn finish_tool_call(
    handle: tokio::task::JoinHandle<anyhow::Result<serde_json::Value>>,
) -> serde_json::Value {
    tokio::time::timeout(EVENT_TIMEOUT, handle)
        .await
        .expect("tool call timed out")
        .expect("tool task panicked")
        .expect("tool call failed")
}

#[tokio::test]
async fn host_read_waits_for_same_path_operation_and_sees_completed_version() {
    let dir = tempfile::tempdir().expect("temp directory");
    let path = dir.path().join("shared.txt");
    tokio::fs::write(&path, "before\n")
        .await
        .expect("write initial file");
    let canonical = tokio::fs::canonicalize(&path)
        .await
        .expect("canonicalize test file");
    let lock_key = host_fs_operation_lock_key(&canonical);

    let (holder_entered_tx, holder_entered_rx) = oneshot::channel();
    let (finish_mutation_tx, finish_mutation_rx) = oneshot::channel();
    let mutation_path = path.clone();
    let mutation = tokio::spawn(async move {
        with_fs_operation_lock(lock_key, async move {
            holder_entered_tx.send(()).expect("signal lock holder");
            finish_mutation_rx.await.expect("continue mutation");
            tokio::fs::write(mutation_path, "after\n")
                .await
                .expect("write completed version");
        })
        .await;
    });
    holder_entered_rx.await.expect("wait for lock holder");

    let read_path = path.to_string_lossy().into_owned();
    let mut read = tokio::spawn(async move {
        ReadTool::new()
            .execute(json!({ "file_path": read_path, "limit": 10 }))
            .await
    });
    assert!(
        tokio::time::timeout(NO_ENTRY_WINDOW, &mut read)
            .await
            .is_err(),
        "Read must wait while the matching filesystem operation lock is held"
    );

    finish_mutation_tx.send(()).expect("finish mutation");
    mutation.await.expect("mutation task");
    let value = finish_tool_call(read).await;
    assert!(value["content"].as_str().unwrap().contains("after"));
    assert!(!value["content"].as_str().unwrap().contains("before"));
}

#[tokio::test]
async fn same_turn_sandbox_write_precedes_read_and_read_sees_written_version() {
    let path = "/home/sandbox/shared/topics/writer-priority.txt";
    let session_key = "writer-priority-session";
    let (backend, mut reads) = CoordinatedSandbox::new([(path.to_string(), b"before\n".to_vec())]);
    let router = sandbox_router(Arc::clone(&backend));
    let read_tool = ReadTool::new().with_sandbox_router(Arc::clone(&router));
    let write_tool = WriteTool::new().with_sandbox_router(router);

    let calls = tokio::spawn(async move {
        tokio::join!(
            read_tool.execute(json!({
                "file_path": path,
                "limit": 10,
                "_session_key": session_key,
            })),
            write_tool.execute(json!({
                "file_path": path,
                "content": "after\n",
                "_session_key": session_key,
            })),
        )
    });

    backend.wait_for_preparation_calls(1).await;
    assert_eq!(backend.preparation_calls.load(Ordering::SeqCst), 1);
    assert!(
        tokio::time::timeout(NO_ENTRY_WINDOW, backend.preparation_changed.notified())
            .await
            .is_err(),
        "Read reached sandbox preparation before the same-turn Write completed"
    );

    backend.open_preparation();
    assert_eq!(next_read_started(&mut reads).await, path);
    assert_eq!(backend.file_content(path), b"after\n");
    backend.release_reads(1);

    let (read_result, write_result) = tokio::time::timeout(EVENT_TIMEOUT, calls)
        .await
        .expect("same-turn tool calls timed out")
        .expect("same-turn tool task panicked");
    let read_value = read_result.expect("Read succeeds");
    let write_value = write_result.expect("Write succeeds");
    assert_eq!(write_value["bytes_written"], 6);
    assert!(read_value["content"].as_str().unwrap().contains("after"));
    assert!(!read_value["content"].as_str().unwrap().contains("before"));
}

#[tokio::test]
async fn parallel_sandbox_reads_of_same_path_are_serialized() {
    let path = "/home/sandbox/shared/topics/same.txt";
    let (backend, mut reads) = CoordinatedSandbox::new([(path.to_string(), b"content\n".to_vec())]);
    let tool = Arc::new(ReadTool::new().with_sandbox_router(sandbox_router(Arc::clone(&backend))));

    let first_tool = Arc::clone(&tool);
    let first = tokio::spawn(async move {
        first_tool
            .execute(json!({
                "file_path": path,
                "limit": 10,
                "_session_key": "same-path-session",
            }))
            .await
    });
    let second_tool = Arc::clone(&tool);
    let second = tokio::spawn(async move {
        second_tool
            .execute(json!({
                "file_path": path,
                "limit": 10,
                "_session_key": "same-path-session",
            }))
            .await
    });

    backend.wait_for_preparation_calls(1).await;
    assert_eq!(backend.preparation_calls.load(Ordering::SeqCst), 1);
    backend.open_preparation();
    assert_eq!(next_read_started(&mut reads).await, path);
    assert!(
        tokio::time::timeout(NO_ENTRY_WINDOW, reads.recv())
            .await
            .is_err(),
        "a second transport read of the same sandbox path entered concurrently"
    );

    backend.release_reads(1);
    assert_eq!(next_read_started(&mut reads).await, path);
    assert_eq!(backend.preparation_calls.load(Ordering::SeqCst), 2);
    backend.release_reads(1);
    finish_tool_call(first).await;
    finish_tool_call(second).await;
    assert_eq!(backend.max_active_reads(), 1);
}

#[tokio::test]
async fn sandbox_reads_of_different_paths_remain_parallel() {
    let first_path = "/home/sandbox/shared/topics/first.txt";
    let second_path = "/home/sandbox/shared/topics/second.txt";
    let (backend, mut reads) = CoordinatedSandbox::new([
        (first_path.to_string(), b"first\n".to_vec()),
        (second_path.to_string(), b"second\n".to_vec()),
    ]);
    let tool = Arc::new(ReadTool::new().with_sandbox_router(sandbox_router(Arc::clone(&backend))));

    let first_tool = Arc::clone(&tool);
    let first = tokio::spawn(async move {
        first_tool
            .execute(json!({
                "file_path": first_path,
                "limit": 10,
                "_session_key": "different-path-session",
            }))
            .await
    });
    let second_tool = Arc::clone(&tool);
    let second = tokio::spawn(async move {
        second_tool
            .execute(json!({
                "file_path": second_path,
                "limit": 10,
                "_session_key": "different-path-session",
            }))
            .await
    });

    backend.wait_for_preparation_calls(2).await;
    backend.open_preparation();
    let mut started = vec![
        next_read_started(&mut reads).await,
        next_read_started(&mut reads).await,
    ];
    started.sort();
    assert_eq!(started, vec![
        first_path.to_string(),
        second_path.to_string()
    ]);
    assert_eq!(backend.max_active_reads(), 2);

    backend.release_reads(2);
    finish_tool_call(first).await;
    finish_tool_call(second).await;
}

#[tokio::test]
async fn auto_paged_read_holds_lock_until_edit_can_observe_one_complete_snapshot() {
    let path = "/home/sandbox/shared/topics/large.txt";
    let content = (1..=3_000)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (backend, mut reads) = CoordinatedSandbox::new([(path.to_string(), content.into_bytes())]);
    let router = sandbox_router(Arc::clone(&backend));

    let read_tool = ReadTool::new().with_sandbox_router(Arc::clone(&router));
    let read = tokio::spawn(async move {
        read_tool
            .execute(json!({
                "file_path": path,
                "_session_key": "auto-page-session",
            }))
            .await
    });

    backend.wait_for_preparation_calls(1).await;
    backend.open_preparation();
    assert_eq!(next_read_started(&mut reads).await, path);
    backend.release_reads(1);
    assert_eq!(next_read_started(&mut reads).await, path);

    let edit_tool = EditTool::new().with_sandbox_router(router);
    let edit = tokio::spawn(async move {
        edit_tool
            .execute(json!({
                "file_path": path,
                "old_string": "line 3000",
                "new_string": "edited line 3000",
                "_session_key": "auto-page-session",
            }))
            .await
    });
    assert!(
        tokio::time::timeout(NO_ENTRY_WINDOW, reads.recv())
            .await
            .is_err(),
        "Edit entered its transport read between auto-paged Read pages"
    );
    assert_eq!(
        backend.preparation_calls.load(Ordering::SeqCst),
        1,
        "Edit reached sandbox preparation before auto-paged Read released its request lock"
    );

    backend.release_reads(1);
    let read_value = finish_tool_call(read).await;
    assert_eq!(read_value["rendered_lines"], 3_000);
    assert!(
        read_value["content"]
            .as_str()
            .unwrap()
            .contains("line 3000")
    );

    backend.wait_for_preparation_calls(2).await;
    assert_eq!(next_read_started(&mut reads).await, path);
    backend.release_reads(1);
    let edit_value = finish_tool_call(edit).await;
    assert_eq!(edit_value["replacements"], 1);
    assert!(
        String::from_utf8(backend.file_content(path))
            .expect("sandbox file stays UTF-8")
            .contains("edited line 3000")
    );
}
