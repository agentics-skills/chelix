#[path = "tests/core.rs"]
mod core;
#[path = "tests/layered.rs"]
mod layered;

use std::{path::PathBuf, sync::Mutex};

use super::{
    config_io::{load_config_source, parse_config, resubstitute_config, save_user_config_to_path},
    *,
};

struct TestDirState {
    _path: Option<PathBuf>,
}

static DATA_DIR_TEST_LOCK: Mutex<TestDirState> = Mutex::new(TestDirState { _path: None });

/// Lock guarding tests that modify `CONFIG_DIR_OVERRIDE` to prevent races.
static CONFIG_DIR_TEST_LOCK: Mutex<TestDirState> = Mutex::new(TestDirState { _path: None });

#[test]
fn explicit_config_dir_prefers_programmatic_override_and_ignores_empty_env() {
    let programmatic = PathBuf::from("/programmatic");
    let env = PathBuf::from("/environment");

    assert_eq!(
        resolve_explicit_config_dir(Some(programmatic.clone()), Some(env.clone()))
            .expect("explicit dir"),
        Some(programmatic)
    );
    assert_eq!(
        resolve_explicit_config_dir(None, Some(env.clone())).expect("explicit dir"),
        Some(env)
    );
    assert_eq!(
        resolve_explicit_config_dir(None, Some(PathBuf::new())).expect("explicit dir"),
        None
    );
}
