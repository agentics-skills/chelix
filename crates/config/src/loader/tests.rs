#[path = "tests/core.rs"]
mod core;
#[path = "tests/layered.rs"]
mod layered;

use std::{path::PathBuf, sync::Mutex};

use super::{
    config_io::{
        apply_env_overrides_with, find_config_file_from_dirs, parse_config, parse_env_value,
        resubstitute_config, save_user_config_to_path, set_nested, strip_default_values,
    },
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
    let programmatic = PathBuf::from("programmatic");
    let env = PathBuf::from("environment");

    assert_eq!(
        resolve_explicit_config_dir(Some(programmatic.clone()), Some(env.clone())),
        Some(programmatic)
    );
    assert_eq!(
        resolve_explicit_config_dir(None, Some(env.clone())),
        Some(env)
    );
    assert_eq!(
        resolve_explicit_config_dir(None, Some(PathBuf::new())),
        None
    );
}

#[test]
fn explicit_config_dir_isolates_config_file_discovery() {
    let root = tempfile::tempdir().expect("tempdir");
    let explicit_dir = root.path().join("explicit");
    let project_dir = root.path().join("project");
    let default_dir = root.path().join("home/.config/chelix");
    std::fs::create_dir_all(&explicit_dir).expect("create explicit dir");
    std::fs::create_dir_all(&project_dir).expect("create project dir");
    std::fs::create_dir_all(&default_dir).expect("create default dir");

    let project_config = project_dir.join("chelix.toml");
    let default_config = default_dir.join("chelix.toml");
    std::fs::write(&project_config, "").expect("write project config");
    std::fs::write(&default_config, "").expect("write default config");

    assert_eq!(
        find_config_file_from_dirs(
            Some(&explicit_dir),
            &project_dir,
            Some(default_dir.as_path()),
        ),
        None
    );

    let explicit_config = explicit_dir.join("chelix.toml");
    std::fs::write(&explicit_config, "").expect("write explicit config");
    assert_eq!(
        find_config_file_from_dirs(
            Some(&explicit_dir),
            &project_dir,
            Some(default_dir.as_path()),
        ),
        Some(explicit_config)
    );
}
