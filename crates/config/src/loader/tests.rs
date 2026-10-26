#[path = "tests/codec.rs"]
mod codec;
#[path = "tests/core.rs"]
mod core;

use std::{path::PathBuf, sync::Mutex};

use super::*;

struct TestDirState {
    _path: Option<PathBuf>,
}

static DATA_DIR_TEST_LOCK: Mutex<TestDirState> = Mutex::new(TestDirState { _path: None });

#[test]
fn explicit_config_dir_requires_absolute_env_and_prefers_programmatic_override() {
    let programmatic = PathBuf::from("/programmatic");
    let env = PathBuf::from("/environment");

    assert_eq!(
        resolve_explicit_config_dir(Some(programmatic.clone()), Some(env.clone()))
            .expect("explicit dir"),
        Some(programmatic.clone())
    );
    assert_eq!(
        resolve_explicit_config_dir(None, Some(env)).expect("explicit dir"),
        Some(PathBuf::from("/environment"))
    );
    assert_eq!(
        resolve_explicit_config_dir(Some(programmatic.clone()), None).expect("explicit dir"),
        Some(programmatic.clone())
    );
    assert_eq!(
        resolve_explicit_config_dir(None, None).expect("explicit dir"),
        None
    );

    assert_eq!(
        resolve_explicit_config_dir(Some(programmatic.clone()), Some(PathBuf::new()))
            .expect("empty environment keeps programmatic override"),
        Some(programmatic),
    );
    assert_eq!(
        resolve_explicit_config_dir(None, Some(PathBuf::new()))
            .expect("empty environment is unset"),
        None,
    );

    let relative = resolve_explicit_config_dir(None, Some(PathBuf::from("relative/dir")))
        .expect_err("relative CHELIX_CONFIG_DIR");
    let relative = relative.to_string();
    assert!(relative.contains("absolute"), "{relative}");
    assert!(relative.contains("relative/dir"), "{relative}");
}
