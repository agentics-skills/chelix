use std::sync::{Arc, OnceLock};

use chelix_config::{ChelixConfig, Diagnostic};

use crate::{BlockingValidator, ConfigValidator};

#[tokio::test]
async fn blocking_validator_executes_on_a_worker_thread() {
    static CALLER_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();

    fn check(_: &ChelixConfig) -> Vec<Diagnostic> {
        assert_ne!(CALLER_THREAD.get(), Some(&std::thread::current().id()));
        Vec::new()
    }

    assert!(CALLER_THREAD.set(std::thread::current().id()).is_ok());
    let diagnostics = BlockingValidator(check)
        .validate(Arc::new(ChelixConfig::default()))
        .await;
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
