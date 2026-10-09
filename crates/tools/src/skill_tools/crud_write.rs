#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

struct CheckingWriteRecorder {
    data_dir: PathBuf,
    store: chelix_skills::usage::SkillUsageStore,
    expected_allow: Vec<String>,
    expected_body: String,
}

#[async_trait::async_trait]
impl helpers::SkillWriteRecorder for CheckingWriteRecorder {
    async fn record_write(&self, name: &str) {
        let dir = self.data_dir.join("skills").join(name);
        let raw = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        let content = chelix_skills::parse::parse_skill(&raw, &dir).unwrap();
        assert_eq!(content.metadata.allow, self.expected_allow);
        assert!(content.metadata.deny.is_empty());
        assert_eq!(content.body, self.expected_body);
        self.store.record_write(name).await;
    }
}

#[tokio::test]
async fn create_and_update_publish_access_before_usage_accounting() {
    use {
        chelix_agents::tool_context::ToolExecutionContext, chelix_skills::usage::SkillUsageStore,
        std::sync::Arc,
    };

    let tmp = tempfile::tempdir().unwrap();
    let store = SkillUsageStore::open(tmp.path()).await;
    for (name, agent_id) in [("owned", Some("agent1")), ("shared", None)] {
        let recorder = Arc::new(CheckingWriteRecorder {
            data_dir: tmp.path().to_path_buf(),
            store: store.clone(),
            expected_allow: agent_id.into_iter().map(String::from).collect(),
            expected_body: "created body".into(),
        });
        let create =
            CreateSkillTool::new(tmp.path().to_path_buf()).with_write_recorder(recorder.clone());
        let params = json!({ "name": name, "description": "test", "body": "created body" });
        if let Some(agent_id) = agent_id {
            let context = ToolExecutionContext::for_session_with_agent(
                chelix_sessions::SessionKey::new("session:test"),
                agent_id,
            );
            create.execute_with_context(params, &context).await.unwrap();
        } else {
            create.execute(params).await.unwrap();
        }
        assert_eq!(recorder.store.get_all().await[name].write_count, 1);
    }
    let name = "updated";
    let dir = tmp.path().join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: updated\nallow: [agent1]\ndeny: []\n---\nold body",
    )
    .unwrap();
    let recorder = Arc::new(CheckingWriteRecorder {
        data_dir: tmp.path().to_path_buf(),
        store,
        expected_allow: vec!["agent1".into()],
        expected_body: "updated body".into(),
    });
    let update =
        UpdateSkillTool::new(tmp.path().to_path_buf()).with_write_recorder(recorder.clone());
    update
        .execute(json!({ "name": name, "description": "test", "body": "updated body" }))
        .await
        .unwrap();
    assert_eq!(recorder.store.get_all().await[name].write_count, 1);
}

#[tokio::test]
async fn personal_mutations_reject_invalid_frontmatter_before_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skills/demo");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    let original = "---\nname: demo\nallow:\n---\nbody\n";
    std::fs::write(&path, original).unwrap();
    let tools: Vec<Box<dyn AgentTool>> = vec![
        Box::new(CreateSkillTool::new(tmp.path().to_path_buf())),
        Box::new(UpdateSkillTool::new(tmp.path().to_path_buf())),
        Box::new(PatchSkillTool::new(tmp.path().to_path_buf())),
        Box::new(DeleteSkillTool::new(tmp.path().to_path_buf())),
        Box::new(WriteSkillFilesTool::new(tmp.path().to_path_buf())),
    ];
    for tool in tools {
        let error = tool
            .execute(json!({
                "name": "demo", "description": "test", "body": "changed",
                "patches": [{ "find": "body", "replace": "changed" }],
                "files": [{ "path": "reference.md", "content": "changed" }]
            }))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("frontmatter"),
            "{}: {error}",
            tool.name()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }
}

#[tokio::test]
async fn patch_and_sidecar_write_publish_empty_access_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skills/demo");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    let original = "---\nname: demo\ndescription: test\n---\n\noriginal body\n";
    std::fs::write(&path, original).unwrap();
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());
    patch
        .execute(
            json!({ "name": "demo", "patches": [{ "find": "original", "replace": "patched" }] }),
        )
        .await
        .unwrap();
    let patched = std::fs::read_to_string(&path).unwrap();
    assert!(patched.contains("allow: []"));
    assert!(patched.contains("deny: []"));
    assert!(patched.ends_with("\n\npatched body\n"));
    std::fs::write(&path, original).unwrap();
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());
    write.execute(json!({ "name": "demo", "files": [{ "path": "references/api.md", "content": "reference" }] })).await.unwrap();
    let published = std::fs::read_to_string(&path).unwrap();
    assert!(published.contains("allow: []"));
    assert!(published.contains("deny: []"));
    assert!(published.ends_with("\n\noriginal body\n"));
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_symlink_write_preserves_link_and_updates_target() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skills/demo");
    std::fs::create_dir_all(dir.join("references")).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: demo\n---\nbody").unwrap();
    let target = outside.path().join("api.md");
    std::fs::write(&target, "original").unwrap();
    let link = dir.join("references/api.md");
    symlink(&target, &link).unwrap();
    let tool = WriteSkillFilesTool::new(tmp.path().to_path_buf());
    tool.execute(
        json!({ "name": "demo", "files": [{ "path": "references/api.md", "content": "updated" }] }),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "updated");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn test_create_with_allowed_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = CreateSkillTool::new(tmp.path().to_path_buf());

    tool.execute(json!({
        "name": "git-skill",
        "description": "Use when: X\nNext step",
        "body": "Help with git.",
        "allowed_tools": ["Bash(git:*)", "Tool: action\nNext"]
    }))
    .await
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("skills/git-skill/SKILL.md")).unwrap();
    let metadata =
        chelix_skills::parse::parse_metadata(&content, &tmp.path().join("skills/git-skill"))
            .unwrap();
    assert_eq!(metadata.description, "Use when: X\nNext step");
    assert_eq!(metadata.allowed_tools, [
        "Bash(git:*)",
        "Tool: action\nNext"
    ]);
}

#[tokio::test]
async fn test_create_invalid_name() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = CreateSkillTool::new(tmp.path().to_path_buf());

    let result = tool
        .execute(json!({
            "name": "Bad Name",
            "description": "test",
            "body": "body"
        }))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_create_duplicate_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = CreateSkillTool::new(tmp.path().to_path_buf());

    tool.execute(json!({
        "name": "my-skill",
        "description": "test",
        "body": "body"
    }))
    .await
    .unwrap();

    let result = tool
        .execute(json!({
            "name": "my-skill",
            "description": "test2",
            "body": "body2"
        }))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_update_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let update = UpdateSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "original",
            "body": "original body"
        }))
        .await
        .unwrap();

    let result = update
        .execute(json!({
            "name": "my-skill",
            "description": "updated",
            "body": "new body"
        }))
        .await
        .unwrap();
    assert!(result["updated"].as_bool().unwrap());

    let content = std::fs::read_to_string(tmp.path().join("skills/my-skill/SKILL.md")).unwrap();
    assert!(content.contains("description: updated"));
    assert!(content.contains("new body"));
}

#[tokio::test]
async fn test_update_nonexistent_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = UpdateSkillTool::new(tmp.path().to_path_buf());

    let result = tool
        .execute(json!({
            "name": "nope",
            "description": "test",
            "body": "body"
        }))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_delete_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let delete = DeleteSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = delete.execute(json!({ "name": "my-skill" })).await.unwrap();
    assert!(result["deleted"].as_bool().unwrap());
    assert!(!tmp.path().join("skills/my-skill").exists());
}

#[tokio::test]
async fn test_delete_nonexistent_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = DeleteSkillTool::new(tmp.path().to_path_buf());

    let result = tool.execute(json!({ "name": "nope" })).await;
    assert!(result.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn test_delete_skill_removes_alias_and_preserves_target() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    let real_dir = skills_dir.join("real-skill");
    std::fs::create_dir_all(&real_dir).unwrap();
    let original = "---\nname: real-skill\ndescription: test\n---\nreal content\n";
    std::fs::write(real_dir.join("SKILL.md"), original).unwrap();
    let alias_dir = skills_dir.join("alias-skill");
    symlink(&real_dir, &alias_dir).unwrap();

    let tool = DeleteSkillTool::new(tmp.path().to_path_buf());
    let result = tool.execute(json!({ "name": "alias-skill" })).await;

    assert_eq!(result.unwrap()["deleted"], true);
    assert!(std::fs::symlink_metadata(&alias_dir).is_err());
    assert_eq!(
        std::fs::read_to_string(real_dir.join("SKILL.md")).unwrap(),
        original
    );
}

#[tokio::test]
async fn test_write_skill_files_writes_sidecars_and_audits() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = write
        .execute(json!({
            "name": "my-skill",
            "files": [
                { "path": "script.sh", "content": "#!/usr/bin/env bash\necho hi\n" },
                { "path": "templates/prompt.txt", "content": "hello\n" },
                { "path": "_meta.json", "content": "{\"owner\":\"me\"}\n" }
            ]
        }))
        .await
        .unwrap();

    assert!(result["written"].as_bool().unwrap());
    assert_eq!(result["files_written"].as_u64().unwrap(), 3);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("skills/my-skill/script.sh")).unwrap(),
        "#!/usr/bin/env bash\necho hi\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("skills/my-skill/templates/prompt.txt")).unwrap(),
        "hello\n"
    );

    let audit_log = std::fs::read_to_string(tmp.path().join("logs/security-audit.jsonl")).unwrap();
    assert!(audit_log.contains("\"event\":\"skills.sidecar_files.write\""));
    assert!(audit_log.contains("\"path\":\"script.sh\""));
}

#[tokio::test]
async fn test_write_skill_files_requires_existing_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    let result = write
        .execute(json!({
            "name": "missing-skill",
            "files": [{ "path": "script.sh", "content": "echo hi\n" }]
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_skill_files_rejects_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = write
        .execute(json!({
            "name": "my-skill",
            "files": [{ "path": "../escape.sh", "content": "echo nope\n" }]
        }))
        .await;

    assert!(result.is_err());
    assert!(!tmp.path().join("skills/escape.sh").exists());
}

#[tokio::test]
async fn test_write_skill_files_rejects_reserved_skill_md() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = write
        .execute(json!({
            "name": "my-skill",
            "files": [{ "path": "SKILL.md", "content": "nope\n" }]
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_skill_files_rejects_hidden_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = write
        .execute(json!({
            "name": "my-skill",
            "files": [{ "path": ".secret", "content": "nope\n" }]
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_skill_files_rejects_duplicate_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = write
        .execute(json!({
            "name": "my-skill",
            "files": [
                { "path": "script.sh", "content": "echo one\n" },
                { "path": "script.sh", "content": "echo two\n" }
            ]
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_skill_files_rejects_oversize_file() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = write
        .execute(json!({
            "name": "my-skill",
            "files": [{
                "path": "huge.txt",
                "content": "x".repeat(MAX_SIDECAR_FILE_BYTES + 1)
            }]
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_delete_skill_removes_sidecar_files() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());
    let delete = DeleteSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();
    write
        .execute(json!({
            "name": "my-skill",
            "files": [{ "path": "script.sh", "content": "echo hi\n" }]
        }))
        .await
        .unwrap();

    delete.execute(json!({ "name": "my-skill" })).await.unwrap();
    assert!(!tmp.path().join("skills/my-skill").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_skill_files_follows_symlinked_skill_root() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();

    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let real_dir = outside.path().join("real-skill");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::write(real_dir.join("SKILL.md"), "---\nname: demo\n---\n").unwrap();
    symlink(&real_dir, skills_dir.join("demo")).unwrap();

    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());
    let result = write
        .execute(json!({
            "name": "demo",
            "files": [{ "path": "script.sh", "content": "echo example\n" }]
        }))
        .await;

    assert!(result.is_ok());
    assert_eq!(
        std::fs::read_to_string(real_dir.join("script.sh")).unwrap(),
        "echo example\n"
    );
}

// ── PatchSkillTool tests ────────────────────────────────────────────────

#[tokio::test]
async fn test_patch_skill_single_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "A test skill",
            "body": "Step 1: Do X\nStep 2: Do Y\nStep 3: Do Z"
        }))
        .await
        .unwrap();

    let result = patch
        .execute(json!({
            "name": "my-skill",
            "patches": [
                { "find": "Do Y", "replace": "Do B" }
            ]
        }))
        .await
        .unwrap();

    assert!(result["patched"].as_bool().unwrap());
    assert_eq!(result["patches_applied"].as_u64().unwrap(), 1);

    let content = std::fs::read_to_string(tmp.path().join("skills/my-skill/SKILL.md")).unwrap();
    assert!(content.contains("Do B"));
    assert!(content.contains("Do X"));
    assert!(content.contains("Do Z"));
    // Frontmatter should be preserved.
    assert!(content.contains("name: my-skill"));
    assert!(content.contains("description: A test skill"));
}

#[tokio::test]
async fn test_patch_skill_multiple_patches_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "AAA BBB CCC"
        }))
        .await
        .unwrap();

    let result = patch
        .execute(json!({
            "name": "my-skill",
            "patches": [
                { "find": "AAA", "replace": "111" },
                { "find": "BBB", "replace": "222" },
                { "find": "CCC", "replace": "333" }
            ]
        }))
        .await
        .unwrap();

    assert_eq!(result["patches_applied"].as_u64().unwrap(), 3);
    let content = std::fs::read_to_string(tmp.path().join("skills/my-skill/SKILL.md")).unwrap();
    assert!(content.contains("111 222 333"));
}

#[tokio::test]
async fn test_patch_skill_find_not_found_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "Hello world"
        }))
        .await
        .unwrap();

    let result = patch
        .execute(json!({
            "name": "my-skill",
            "patches": [
                { "find": "NOTFOUND", "replace": "oops" }
            ]
        }))
        .await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"), "error: {err_msg}");
}

#[tokio::test]
async fn test_patch_skill_nonexistent_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());

    let result = patch
        .execute(json!({
            "name": "nope",
            "patches": [{ "find": "a", "replace": "b" }]
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_patch_skill_invalid_name_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());

    let result = patch
        .execute(json!({
            "name": "Bad Name",
            "patches": [{ "find": "a", "replace": "b" }]
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_patch_skill_empty_patches_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = patch
        .execute(json!({
            "name": "my-skill",
            "patches": []
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_patch_skill_updates_description_after_multiline_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skills/my-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    std::fs::write(&path, "---\nname: my-skill\ndescription: \"first\\nsecond\"\nallow: [agent1]\ndeny: [agent2]\n---\n\nHello world\n").unwrap();
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());
    patch
        .execute(
            json!({ "name": "my-skill", "patches": [{ "find": "Hello", "replace": "Welcome" }] }),
        )
        .await
        .unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        chelix_skills::parse::parse_metadata(&first, &dir)
            .unwrap()
            .description,
        "first\nsecond"
    );
    patch.execute(json!({ "name": "my-skill", "patches": [{ "find": "Welcome", "replace": "Goodbye" }], "description": "new desc" })).await.unwrap();
    let content = std::fs::read_to_string(path).unwrap();
    let parsed = chelix_skills::parse::parse_skill(&content, &dir).unwrap();
    assert_eq!(parsed.metadata.description, "new desc");
    assert_eq!(parsed.metadata.allow, ["agent1"]);
    assert_eq!(parsed.metadata.deny, ["agent2"]);
    assert_eq!(parsed.body, "Goodbye world");
}

#[tokio::test]
async fn test_patch_skill_empty_find_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let patch = PatchSkillTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    let result = patch
        .execute(json!({
            "name": "my-skill",
            "patches": [{ "find": "", "replace": "oops" }]
        }))
        .await;

    assert!(result.is_err());
}

// ── split_frontmatter_body / update_frontmatter_description tests ───────

#[test]
fn test_split_frontmatter_body_with_frontmatter() {
    let raw = "---\nname: foo\ndescription: bar\n---\n\nBody text here.";
    let (fm, body) = split_frontmatter_body(raw);
    assert!(fm.starts_with("---"));
    assert!(fm.contains("name: foo"));
    assert_eq!(body, "Body text here.");
}

#[test]
fn test_split_frontmatter_body_no_frontmatter() {
    let raw = "Just a body.";
    let (fm, body) = split_frontmatter_body(raw);
    assert_eq!(fm, "");
    assert_eq!(body, "Just a body.");
}

#[test]
fn test_split_frontmatter_body_unclosed_frontmatter() {
    let raw = "---\nname: foo\nno closing delimiter";
    let (fm, body) = split_frontmatter_body(raw);
    // Unclosed frontmatter is treated as no frontmatter.
    assert_eq!(fm, "");
    assert_eq!(body, raw);
}

#[test]
fn test_split_frontmatter_body_empty_frontmatter() {
    let raw = "---\n---\nBody after empty frontmatter.";
    let (fm, body) = split_frontmatter_body(raw);
    assert!(fm.contains("---\n---"));
    assert_eq!(body, "Body after empty frontmatter.");
}

#[test]
fn test_split_frontmatter_body_no_trailing_newline() {
    let raw = "---\nname: x\n---";
    let (fm, body) = split_frontmatter_body(raw);
    assert!(fm.contains("---\nname: x\n---"));
    assert_eq!(body, "");
}

#[test]
fn test_update_frontmatter_description_replaces_yaml_value() {
    let fm = "---\nname: foo\ndescription: old\n---\n\n";
    for description in [
        "new desc",
        "has: colons and # hashes",
        r#"says "hello""#,
        "first\nsecond",
    ] {
        let result = update_frontmatter_description(fm, description).unwrap();
        let metadata = chelix_skills::parse::parse_metadata(&result, Path::new("foo")).unwrap();
        assert_eq!(metadata.description, description);
        assert_eq!(metadata.name, "foo");
        assert!(result.ends_with("\n\n"));
    }
}

#[test]
fn test_update_frontmatter_description_missing_field() {
    let fm = "---\nname: foo\n---\n\n";
    let result = update_frontmatter_description(fm, "new desc").unwrap();
    assert_eq!(result, fm);
}

#[cfg(unix)]
#[tokio::test]
async fn test_patch_skill_follows_symlinked_skill_root() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();

    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let real_dir = outside.path().join("real-skill");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::write(
        real_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: example\n---\n\noriginal body",
    )
    .unwrap();
    symlink(&real_dir, skills_dir.join("demo")).unwrap();

    let patch = PatchSkillTool::new(tmp.path().to_path_buf());
    let result = patch
        .execute(json!({
            "name": "demo",
            "patches": [{ "find": "original", "replace": "updated" }]
        }))
        .await;

    assert!(result.is_ok());
    let content = std::fs::read_to_string(real_dir.join("SKILL.md")).unwrap();
    assert!(content.contains("updated body"));
}

#[tokio::test]
async fn test_write_skill_files_keeps_first_file_on_error() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(tmp.path().to_path_buf());
    let write = WriteSkillFilesTool::new(tmp.path().to_path_buf());

    create
        .execute(json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }))
        .await
        .unwrap();

    // The second target is a directory.
    let collision_dir = tmp.path().join("skills/my-skill/collision");
    std::fs::create_dir_all(&collision_dir).unwrap();

    let result = write
        .execute(json!({
            "name": "my-skill",
            "files": [
                { "path": "first.txt", "content": "ok\n" },
                { "path": "collision", "content": "boom\n" }
            ]
        }))
        .await;

    assert_eq!(
        result.unwrap_err().to_string(),
        "target 'collision' is a directory"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("skills/my-skill/first.txt")).unwrap(),
        "ok\n"
    );
}
