#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Skill consistency tests (plan §7.4).
//!
//! Skills are host-managed under `data_dir/skills` (single source of truth),
//! while the sandbox sees the same files through the mandatory read-write
//! `data_dir` mount at the identical guest path. Skill scripts execute through
//! the resolved environment: inside the container when sandboxing is enabled,
//! on the host only when sandboxing is explicitly disabled.

use std::sync::Arc;

use {chelix_agents::tool_registry::AgentTool, serde_json::json};

use {
    super::*,
    crate::{
        command::run_shell_command,
        sandbox::{
            ExecEnv,
            file_system::test_helpers::MockSandbox,
            paths::{MountAccess, resolve_sandbox_mount_path, resolved_sandbox_mount_plan},
        },
        skill_tools::{CreateSkillTool, PatchSkillTool, WriteSkillFilesTool},
    },
};

fn skills_test_sandbox_id() -> SandboxId {
    SandboxId {
        scope: SandboxScope::Session,
        key: "skills-sess".into(),
    }
}

fn data_mount_plan(
    host_data_dir: &std::path::Path,
) -> Vec<chelix_config::container_mounts::SandboxMount> {
    let config = SandboxConfig {
        host_data_dir: Some(host_data_dir.to_path_buf()),
        home_persistence: HomePersistence::Off,
        ..Default::default()
    };
    resolved_sandbox_mount_plan(&config, Some("docker"), &skills_test_sandbox_id()).unwrap()
}

async fn create_test_skill(data_dir: &std::path::Path, name: &str, body: &str) {
    let create = CreateSkillTool::new(data_dir.to_path_buf());
    create
        .execute(json!({
            "name": name,
            "description": "sandbox consistency test skill",
            "body": body,
        }))
        .await
        .unwrap();
}

#[tokio::test]
async fn create_skill_is_visible_in_sandbox_through_data_dir_mount() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().to_path_buf();

    create_test_skill(&host_data_dir, "sandbox-skill", "Run scripts/run.sh").await;
    let write_files = WriteSkillFilesTool::new(host_data_dir.clone());
    write_files
        .execute(json!({
            "name": "sandbox-skill",
            "files": [{
                "path": "scripts/run.sh",
                "content": "#!/usr/bin/env bash\necho skill-ok\n",
            }],
        }))
        .await
        .unwrap();

    let mounts = data_mount_plan(&host_data_dir);
    let guest_data_dir = chelix_config::data_dir();

    // The skill and its script resolve to the agent-written host files through
    // the mandatory data_dir mount, at the identical guest path (invariant §2.3).
    for relative in [
        "skills/sandbox-skill/SKILL.md",
        "skills/sandbox-skill/scripts/run.sh",
    ] {
        let host_view =
            resolve_sandbox_mount_path(&mounts, &guest_data_dir.join(relative), MountAccess::Read)
                .unwrap_or_else(|| panic!("{relative} must resolve through the data_dir mount"));
        assert_eq!(host_view, host_data_dir.join(relative));
    }

    let script = resolve_sandbox_mount_path(
        &mounts,
        &guest_data_dir.join("skills/sandbox-skill/scripts/run.sh"),
        MountAccess::Read,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(script).unwrap(),
        "#!/usr/bin/env bash\necho skill-ok\n",
        "sandbox must see the exact bytes the skill tools wrote on the host"
    );

    // The data mount stays read-write, so in-sandbox edits land in the same
    // skill directory the agent manages.
    assert_eq!(
        resolve_sandbox_mount_path(
            &mounts,
            &guest_data_dir.join("skills/sandbox-skill/notes.md"),
            MountAccess::Write,
        ),
        Some(host_data_dir.join("skills/sandbox-skill/notes.md")),
        "the mandatory data_dir mount must remain writable for skill paths"
    );
}

#[tokio::test]
async fn patch_skill_change_is_visible_at_same_guest_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().to_path_buf();

    create_test_skill(&host_data_dir, "patched-skill", "original instructions").await;

    let mounts = data_mount_plan(&host_data_dir);
    let guest_skill_md = chelix_config::data_dir().join("skills/patched-skill/SKILL.md");

    let host_view =
        resolve_sandbox_mount_path(&mounts, &guest_skill_md, MountAccess::Read).unwrap();
    assert!(
        std::fs::read_to_string(&host_view)
            .unwrap()
            .contains("original instructions")
    );

    let patch = PatchSkillTool::new(host_data_dir.clone());
    patch
        .execute(json!({
            "name": "patched-skill",
            "patches": [{
                "find": "original instructions",
                "replace": "patched instructions",
            }],
        }))
        .await
        .unwrap();

    let host_view_after =
        resolve_sandbox_mount_path(&mounts, &guest_skill_md, MountAccess::Read).unwrap();
    assert_eq!(
        host_view, host_view_after,
        "the guest path must stay stable across patches"
    );
    let content = std::fs::read_to_string(&host_view_after).unwrap();
    assert!(
        content.contains("patched instructions"),
        "patch_skill changes must be immediately readable at the sandbox guest path"
    );
    assert!(!content.contains("original instructions"));
}

#[tokio::test]
async fn skill_script_executes_in_sandbox_when_enabled() {
    let backend = MockSandbox::new(Vec::new());
    let routed_backend: Arc<dyn Sandbox> = backend.clone();
    let config = SandboxConfig {
        mode: SandboxMode::All,
        ..Default::default()
    };
    let router = SandboxRouter::with_backend(config, routed_backend.clone());

    let script = chelix_config::data_dir().join("skills/sandbox-skill/scripts/run.sh");
    let command = format!("bash '{}'", script.display());

    match router.resolve_env("main").await {
        Ok(ExecEnv::Sandbox {
            backend: resolved_backend,
            id,
        }) => {
            assert!(Arc::ptr_eq(&resolved_backend, &routed_backend));
            let output = resolved_backend
                .run_command(&id, &command, &CommandOptions::default())
                .await
                .unwrap();
            assert_eq!(output.exit_code, 0);
        },
        Ok(ExecEnv::Host) => panic!("skill scripts must not run on the host when sandboxed"),
        Err(error) => panic!("sandbox resolution failed: {error}"),
    }

    assert_eq!(
        backend.last_command(),
        Some(command),
        "the skill script invocation must reach the container backend unchanged"
    );
}

#[tokio::test]
async fn skill_script_executes_on_host_when_sandbox_is_off() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().to_path_buf();

    create_test_skill(&host_data_dir, "host-skill", "Run scripts/run.sh").await;
    let write_files = WriteSkillFilesTool::new(host_data_dir.clone());
    write_files
        .execute(json!({
            "name": "host-skill",
            "files": [{
                "path": "scripts/run.sh",
                "content": "echo skill-on-host\n",
            }],
        }))
        .await
        .unwrap();

    let backend = MockSandbox::new(Vec::new());
    let routed_backend: Arc<dyn Sandbox> = backend.clone();
    let config = SandboxConfig {
        mode: SandboxMode::Off,
        ..Default::default()
    };
    let router = SandboxRouter::with_backend(config, routed_backend);

    let env = router.resolve_env("main").await.unwrap();
    assert!(
        matches!(env, ExecEnv::Host),
        "explicitly disabled sandbox must resolve to host execution"
    );

    // Host execution runs the skill script directly from the agent filesystem.
    let script = host_data_dir.join("skills/host-skill/scripts/run.sh");
    let command = format!("bash '{}'", script.display());
    let output = run_shell_command(&command, &CommandOptions::default())
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout.trim(), "skill-on-host");
    assert!(
        backend.last_command().is_none(),
        "no command must reach the sandbox backend when sandboxing is off"
    );
}
