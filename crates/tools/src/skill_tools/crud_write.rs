#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn skill_bus(root: &Path) -> Arc<chelix_call_bus::CallBus> {
    chelix_skills::skill_file::open_skill_bus_for_agent(
        root,
        "test-agent",
        chelix_config::AgentSkillPolicy::default(),
    )
    .unwrap()
}

struct CheckingWriteRecorder {
    bus: Arc<chelix_call_bus::CallBus>,
    store: chelix_skills::usage::SkillUsageStore,
    expected_allow: Vec<String>,
    expected_body: String,
}

#[async_trait::async_trait]
impl helpers::SkillWriteRecorder for CheckingWriteRecorder {
    async fn record_write(&self, name: &str) {
        let content = self
            .bus
            .call(chelix_call_bus::ReadSkillFile {
                name: name.into(),
                actor: chelix_call_bus::SkillActor::Process,
                locate: chelix_call_bus::SkillLocate::PersonalDir,
                file_path: None,
                max_bytes: None,
            })
            .await
            .unwrap();
        assert_eq!(content.metadata.allowed_agents, self.expected_allow);
        assert!(content.metadata.denied_agents.is_empty());
        assert_eq!(content.body, self.expected_body);
        self.store.record_write(name).await;
    }
}

#[tokio::test]
async fn personal_mutations_reject_invalid_frontmatter_before_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skills/demo");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    let original = "---\nname: demo\nallowed_agents: hello\n---\nbody\n";
    std::fs::write(&path, original).unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let create_error = create
        .execute_with_context(
            json!({ "name": "demo", "description": "test", "body": "changed" }),
            &test_context(),
        )
        .await
        .unwrap_err();
    assert!(
        create_error.to_string().contains("frontmatter"),
        "{create_error}"
    );
    let tools: Vec<Box<dyn AgentTool>> = vec![
        Box::new(UpdateSkillTool::new(skill_bus(tmp.path()))),
        Box::new(PatchSkillTool::new(skill_bus(tmp.path()))),
        Box::new(DeleteSkillTool::new(skill_bus(tmp.path()))),
        Box::new(WriteSkillFilesTool::new(skill_bus(tmp.path()))),
    ];
    for tool in tools {
        let error = tool
            .execute_with_context(
                json!({
                    "name": "demo", "description": "test", "body": "changed",
                    "patches": [{ "find": "body", "replace": "changed" }],
                    "files": [{ "path": "reference.md", "content": "changed" }]
                }),
                &test_context(),
            )
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
async fn test_create_duplicate_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = CreateSkillTool::new(skill_bus(tmp.path()));

    tool.execute_with_context(
        json!({
            "name": "my-skill",
            "description": "test",
            "body": "body"
        }),
        &test_context(),
    )
    .await
    .unwrap();

    let result = tool
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "test2",
                "body": "body2"
            }),
            &test_context(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_update_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let update = UpdateSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "original",
                "body": "original body"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let result = update
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "updated",
                "body": "new body"
            }),
            &test_context(),
        )
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
    let tool = UpdateSkillTool::new(skill_bus(tmp.path()));

    let result = tool
        .execute_with_context(
            json!({
                "name": "nope",
                "description": "test",
                "body": "body"
            }),
            &test_context(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_delete_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let delete = DeleteSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "test",
                "body": "body"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let result = delete
        .execute_with_context(json!({ "name": "my-skill" }), &test_context())
        .await
        .unwrap();
    assert!(result["deleted"].as_bool().unwrap());
    assert!(!tmp.path().join("skills/my-skill").exists());
}

#[tokio::test]
async fn test_delete_nonexistent_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = DeleteSkillTool::new(skill_bus(tmp.path()));

    let result = tool
        .execute_with_context(json!({ "name": "nope" }), &test_context())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_skill_files_requires_existing_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let write = WriteSkillFilesTool::new(skill_bus(tmp.path()));

    let result = write
        .execute_with_context(
            json!({
                "name": "missing-skill",
                "files": [{ "path": "script.sh", "content": "echo hi\n" }]
            }),
            &test_context(),
        )
        .await;

    assert_eq!(
        result.unwrap_err().to_string(),
        "skill 'missing-skill' does not exist; use create_skill first"
    );
}

#[tokio::test]
async fn test_delete_skill_removes_sidecar_files() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let write = WriteSkillFilesTool::new(skill_bus(tmp.path()));
    let delete = DeleteSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "test",
                "body": "body"
            }),
            &test_context(),
        )
        .await
        .unwrap();
    write
        .execute_with_context(
            json!({
                "name": "my-skill",
                "files": [{ "path": "script.sh", "content": "echo hi\n" }]
            }),
            &test_context(),
        )
        .await
        .unwrap();

    delete
        .execute_with_context(json!({ "name": "my-skill" }), &test_context())
        .await
        .unwrap();
    assert!(!tmp.path().join("skills/my-skill").exists());
}

#[tokio::test]
async fn test_patch_skill_single_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let patch = PatchSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "A test skill",
                "body": "Step 1: Do X\nStep 2: Do Y\nStep 3: Do Z"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let result = patch
        .execute_with_context(
            json!({
                "name": "my-skill",
                "patches": [
                    { "find": "Do Y", "replace": "Do B" }
                ]
            }),
            &test_context(),
        )
        .await
        .unwrap();

    assert!(result["patched"].as_bool().unwrap());
    assert_eq!(result["patches_applied"].as_u64().unwrap(), 1);

    let content = std::fs::read_to_string(tmp.path().join("skills/my-skill/SKILL.md")).unwrap();
    assert!(content.contains("Do B"));
    assert!(content.contains("Do X"));
    assert!(content.contains("Do Z"));
    assert!(content.contains("name: my-skill"));
    assert!(content.contains("description: A test skill"));
}

#[tokio::test]
async fn test_patch_skill_multiple_patches_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let patch = PatchSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "test",
                "body": "AAA BBB CCC"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let result = patch
        .execute_with_context(
            json!({
                "name": "my-skill",
                "patches": [
                    { "find": "AAA", "replace": "111" },
                    { "find": "BBB", "replace": "222" },
                    { "find": "CCC", "replace": "333" }
                ]
            }),
            &test_context(),
        )
        .await
        .unwrap();

    assert_eq!(result["patches_applied"].as_u64().unwrap(), 3);
    let content = std::fs::read_to_string(tmp.path().join("skills/my-skill/SKILL.md")).unwrap();
    assert!(content.contains("111 222 333"));
}

#[tokio::test]
async fn test_patch_skill_find_not_found_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let patch = PatchSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "test",
                "body": "Hello world"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let result = patch
        .execute_with_context(
            json!({
                "name": "my-skill",
                "patches": [
                    { "find": "NOTFOUND", "replace": "oops" }
                ]
            }),
            &test_context(),
        )
        .await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"), "error: {err_msg}");
}

#[tokio::test]
async fn test_patch_skill_nonexistent_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let patch = PatchSkillTool::new(skill_bus(tmp.path()));

    let result = patch
        .execute_with_context(
            json!({
                "name": "nope",
                "patches": [{ "find": "a", "replace": "b" }]
            }),
            &test_context(),
        )
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_patch_skill_empty_patches_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let patch = PatchSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "test",
                "body": "body"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let result = patch
        .execute_with_context(
            json!({
                "name": "my-skill",
                "patches": []
            }),
            &test_context(),
        )
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_patch_skill_updates_description_after_multiline_publication() {
    let test_context = || {
        ToolExecutionContext::for_session_with_agent(
            chelix_sessions::SessionKey::new("test"),
            "agent1",
        )
    };
    let tmp = tempfile::tempdir().unwrap();
    let bus = chelix_skills::skill_file::open_skill_bus_for_agent(
        tmp.path(),
        "agent1",
        chelix_config::AgentSkillPolicy::default(),
    )
    .unwrap();
    let dir = tmp.path().join("skills/my-skill");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("SKILL.md");
    std::fs::write(&path, "---\nname: my-skill\ndescription: \"first\\nsecond\"\nallowed_agents:\n- agent1\ndenied_agents:\n- agent2\n---\n\nHello world\n").unwrap();
    let patch = PatchSkillTool::new(Arc::clone(&bus));
    patch
        .execute_with_context(
            json!({ "name": "my-skill", "patches": [{ "find": "Hello", "replace": "Welcome" }] }),
            &test_context(),
        )
        .await
        .unwrap();
    let first = Arc::clone(&bus)
        .call(chelix_call_bus::ReadSkillFile {
            name: "my-skill".into(),
            actor: chelix_call_bus::SkillActor::Process,
            locate: chelix_call_bus::SkillLocate::PersonalDir,
            file_path: None,
            max_bytes: None,
        })
        .await
        .unwrap();
    assert_eq!(first.metadata.description, "first\nsecond");
    patch.execute_with_context(json!({ "name": "my-skill", "patches": [{ "find": "Welcome", "replace": "Goodbye" }], "description": "new desc" }), &test_context()).await.unwrap();
    let parsed = Arc::clone(&bus)
        .call(chelix_call_bus::ReadSkillFile {
            name: "my-skill".into(),
            actor: chelix_call_bus::SkillActor::Process,
            locate: chelix_call_bus::SkillLocate::PersonalDir,
            file_path: None,
            max_bytes: None,
        })
        .await
        .unwrap();
    assert_eq!(parsed.metadata.description, "new desc");
    assert_eq!(parsed.metadata.allowed_agents, ["agent1"]);
    assert_eq!(parsed.metadata.denied_agents, ["agent2"]);
    assert_eq!(parsed.body, "\nGoodbye world\n");
}

#[tokio::test]
async fn test_patch_skill_empty_find_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let create = CreateSkillTool::new(skill_bus(tmp.path()));
    let patch = PatchSkillTool::new(skill_bus(tmp.path()));

    create
        .execute_with_context(
            json!({
                "name": "my-skill",
                "description": "test",
                "body": "body"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let result = patch
        .execute_with_context(
            json!({
                "name": "my-skill",
                "patches": [{ "find": "", "replace": "oops" }]
            }),
            &test_context(),
        )
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn create_and_update_publish_access_before_usage_accounting() {
    use chelix_skills::usage::SkillUsageStore;
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillUsageStore::open(tmp.path()).await;
    let bus = chelix_skills::skill_file::open_skill_bus_for_agent(
        tmp.path(),
        "agent1",
        chelix_config::AgentSkillPolicy::default(),
    )
    .unwrap();
    let context = ToolExecutionContext::for_session_with_agent(
        chelix_sessions::SessionKey::new("test"),
        "agent1",
    );
    let (name, agent_id) = ("owned", Some("agent1"));
    let recorder = Arc::new(CheckingWriteRecorder {
        bus: Arc::clone(&bus),
        store: store.clone(),
        expected_allow: agent_id.into_iter().map(String::from).collect(),
        expected_body: "created body".into(),
    });
    let create = CreateSkillTool::new(Arc::clone(&bus)).with_write_recorder(recorder.clone());
    create
        .execute_with_context(
            json!({ "name": name, "description": "test", "body": "created body" }),
            &context,
        )
        .await
        .unwrap();
    assert_eq!(recorder.store.get_all().await[name].write_count, 1);
    let name = "updated";
    let dir = tmp.path().join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: updated\nallowed_agents:\n- agent1\ndenied_agents:\n---\nold body",
    )
    .unwrap();
    let recorder = Arc::new(CheckingWriteRecorder {
        bus: Arc::clone(&bus),
        store,
        expected_allow: vec!["agent1".into()],
        expected_body: "updated body".into(),
    });
    let update = UpdateSkillTool::new(bus).with_write_recorder(recorder.clone());
    update
        .execute_with_context(
            json!({ "name": name, "description": "test", "body": "updated body" }),
            &context,
        )
        .await
        .unwrap();
    assert_eq!(recorder.store.get_all().await[name].write_count, 1);
}
