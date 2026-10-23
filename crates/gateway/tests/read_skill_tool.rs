#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{path::Path, sync::Arc};

use {
    chelix_agents::tool_registry::{AgentTool, ToolRegistry},
    chelix_call_bus::{ListSkills, SkillSource},
    serde_json::json,
    tempfile::TempDir,
};

fn seed_test_skill(name: &str, body: &str) -> (TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let skill_dir = tmp.path().join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: an integration-test skill\n---\n{body}"),
    )
    .unwrap();
    std::fs::create_dir_all(skill_dir.join("references")).unwrap();
    std::fs::write(
        skill_dir.join("references/notes.md"),
        "# Notes\n\nSome reference material.\n",
    )
    .unwrap();
    (tmp, skill_dir)
}

fn skill_bus(root: &Path) -> Arc<chelix_call_bus::CallBus> {
    chelix_skills::skill_file::open_skill_bus_for_agent(
        root,
        "test-agent",
        chelix_config::AgentSkillPolicy::default(),
    )
    .unwrap()
}

fn test_context() -> chelix_agents::tool_context::ToolExecutionContext {
    chelix_agents::tool_context::ToolExecutionContext::for_session_with_agent(
        chelix_sessions::SessionKey::new("test"),
        "test-agent",
    )
}

fn registry_for(data_dir: &Path) -> ToolRegistry {
    let bus = skill_bus(data_dir);
    let mut registry = ToolRegistry::new();

    registry.register(Box::new(chelix_tools::skill_tools::CreateSkillTool::new(
        Arc::clone(&bus),
    )));
    registry.register(Box::new(chelix_tools::skill_tools::UpdateSkillTool::new(
        Arc::clone(&bus),
    )));
    registry.register(Box::new(chelix_tools::skill_tools::DeleteSkillTool::new(
        Arc::clone(&bus),
    )));
    registry.register(Box::new(
        chelix_tools::skill_tools::WriteSkillFilesTool::new(Arc::clone(&bus)),
    ));

    registry.register(Box::new(chelix_tools::skill_tools::ReadSkillTool::new(bus)));

    registry
}

#[tokio::test]
async fn read_skill_tool_is_registered() {
    let (tmp, _skill_dir) = seed_test_skill("test-skill", "# Test\nHello.\n");
    let registry = registry_for(tmp.path());
    let names = registry.list_names();
    assert!(
        names.iter().any(|name| name == "read_skill"),
        "read_skill must be registered alongside the other skill tools, got: {names:?}"
    );
    assert!(names.iter().any(|name| name == "create_skill"));
    assert!(names.iter().any(|name| name == "update_skill"));
    assert!(names.iter().any(|name| name == "delete_skill"));
}

#[tokio::test]
async fn read_skill_primary_call_returns_body_and_sidecar_listing() {
    let (tmp, _skill_dir) = seed_test_skill("test-skill", "# Test\n\nHello world.\n");
    let registry = registry_for(tmp.path());
    let tool = registry
        .get("read_skill")
        .expect("read_skill is registered");

    let result = tool
        .execute_with_context(json!({ "name": "test-skill" }), &test_context())
        .await
        .expect("reading a seeded skill must succeed");

    assert_eq!(result["name"], "test-skill");
    assert_eq!(result["source"], "personal");
    assert_eq!(result["description"], "an integration-test skill");
    let body = result["body"].as_str().expect("body is a string");
    assert!(
        body.contains("Hello world"),
        "body should contain seeded text: {body:?}"
    );

    let linked: Vec<String> = result["linked_files"]
        .as_array()
        .expect("linked_files is an array")
        .iter()
        .map(|v| v["path"].as_str().unwrap().to_string())
        .collect();
    assert!(
        linked.contains(&"references/notes.md".to_string()),
        "linked_files should list the seeded sidecar: {linked:?}"
    );

    let serialized = serde_json::to_string(&result).unwrap();
    assert!(
        !serialized.contains(tmp.path().to_string_lossy().as_ref()),
        "response must not leak the absolute data_dir path: {serialized}"
    );
}

#[tokio::test]
async fn read_skill_sidecar_call_returns_file_content() {
    let (tmp, _skill_dir) = seed_test_skill("test-skill", "# Test\n");
    let registry = registry_for(tmp.path());
    let tool = registry
        .get("read_skill")
        .expect("read_skill is registered");

    let result = tool
        .execute_with_context(
            json!({
                "name": "test-skill",
                "file_path": "references/notes.md"
            }),
            &test_context(),
        )
        .await
        .expect("reading the sidecar must succeed");

    assert_eq!(result["name"], "test-skill");
    assert_eq!(result["file_path"], "references/notes.md");
    assert_eq!(
        result["content"].as_str().unwrap(),
        "# Notes\n\nSome reference material.\n"
    );
}

#[tokio::test]
async fn read_skill_tool_name_matches_prompt_instruction() {
    let (tmp, _skill_dir) = seed_test_skill("test-skill", "# Test\n");
    let registry = registry_for(tmp.path());

    let tool = registry
        .get(chelix_skills::prompt_gen::READ_SKILL_TOOL_NAME)
        .expect("tool name in prompt must resolve in the registry");
    assert_eq!(tool.name(), chelix_skills::prompt_gen::READ_SKILL_TOOL_NAME);

    let skills = vec![chelix_call_bus::SkillMetadata {
        name: "test-skill".into(),
        description: "an integration-test skill".into(),
        path: tmp.path().join("skills/test-skill"),
        source: Some(SkillSource::Personal),
        ..Default::default()
    }];
    let prompt = chelix_skills::prompt_gen::generate_skills_prompt(&skills);
    assert!(
        prompt.contains(chelix_skills::prompt_gen::READ_SKILL_TOOL_NAME),
        "activation instruction must name `read_skill`: {prompt}"
    );

    let schema = tool.parameters_schema();
    assert_eq!(schema["type"], "object");
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        required.contains(&"name"),
        "primary read must require `name`: {schema}"
    );
    assert!(
        schema["properties"]["file_path"].is_object(),
        "sidecar read must accept optional `file_path`: {schema}"
    );
}

#[tokio::test]
async fn read_skill_resolves_every_name_listed_in_prompt() {
    let tmp = tempfile::tempdir().unwrap();

    for (name, body) in [
        ("alpha", "# Alpha body\n"),
        ("beta", "# Beta body\n"),
        ("gamma", "# Gamma body\n"),
    ] {
        let skill_dir = tmp.path().join("skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test {name}\n---\n{body}"),
        )
        .unwrap();
    }

    let bus = skill_bus(tmp.path());

    let skills = bus
        .call(ListSkills::Agent {
            id: "test-agent".into(),
        })
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.metadata)
        .collect::<Vec<_>>();
    let prompt = chelix_skills::prompt_gen::generate_skills_prompt(&skills);

    let mut listed_names = Vec::new();
    let mut rest = prompt.as_str();
    while let Some(idx) = rest.find("<skill name=\"") {
        rest = &rest[idx + "<skill name=\"".len()..];
        if let Some(end) = rest.find('"') {
            listed_names.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    assert_eq!(
        listed_names.len(),
        3,
        "prompt should list all three seeded skills, got {listed_names:?}"
    );

    let tool = chelix_tools::skill_tools::ReadSkillTool::new(bus);
    for name in &listed_names {
        let result = tool
            .execute_with_context(json!({ "name": name }), &test_context())
            .await
            .unwrap_or_else(|e| panic!("name '{name}' listed in prompt must resolve: {e}"));
        assert_eq!(result["name"], name.as_str());
        assert!(
            result["body"]
                .as_str()
                .unwrap()
                .contains(&format!("{} body", capitalize(name))),
            "body for '{name}' must match seeded content: {result}"
        );
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[tokio::test]
async fn create_skill_then_read_skill_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = registry_for(tmp.path());

    let create = registry
        .get("create_skill")
        .expect("create_skill is registered");
    create
        .execute_with_context(
            json!({
                "name": "round-trip",
                "description": "created via CreateSkillTool",
                "body": "# Round-trip body\n\nHello from create_skill.\n"
            }),
            &test_context(),
        )
        .await
        .expect("create must succeed");

    let read = registry
        .get("read_skill")
        .expect("read_skill is registered");
    let result = read
        .execute_with_context(json!({ "name": "round-trip" }), &test_context())
        .await
        .expect("read must succeed for a just-created skill");
    assert_eq!(result["name"], "round-trip");
    assert_eq!(result["description"], "created via CreateSkillTool");
    assert!(
        result["body"].as_str().unwrap().contains("Round-trip body"),
        "body must reflect create_skill output: {result}"
    );
    assert_eq!(result["source"], "personal");
}

#[tokio::test]
async fn write_skill_files_then_read_skill_sidecar_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = registry_for(tmp.path());

    registry
        .get("create_skill")
        .unwrap()
        .execute_with_context(
            json!({
                "name": "with-sidecars",
                "description": "sidecar round-trip",
                "body": "# Body\n"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    registry
        .get("write_skill_files")
        .unwrap()
        .execute_with_context(
            json!({
                "name": "with-sidecars",
                "files": [
                    { "path": "references/api.md", "content": "# API notes\n" },
                    { "path": "templates/prompt.txt", "content": "hello\n" },
                    { "path": "assets/config.yaml", "content": "k: v\n" }
                ]
            }),
            &test_context(),
        )
        .await
        .expect("write_skill_files must succeed");

    let read = registry.get("read_skill").unwrap();
    let primary = read
        .execute_with_context(json!({ "name": "with-sidecars" }), &test_context())
        .await
        .unwrap();
    let linked: Vec<String> = primary["linked_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap().to_string())
        .collect();
    assert!(linked.contains(&"references/api.md".to_string()));
    assert!(linked.contains(&"templates/prompt.txt".to_string()));
    assert!(linked.contains(&"assets/config.yaml".to_string()));

    for (path, expected) in [
        ("references/api.md", "# API notes\n"),
        ("templates/prompt.txt", "hello\n"),
        ("assets/config.yaml", "k: v\n"),
    ] {
        let result = read
            .execute_with_context(
                json!({ "name": "with-sidecars", "file_path": path }),
                &test_context(),
            )
            .await
            .unwrap_or_else(|e| panic!("sidecar {path} must be readable: {e}"));
        assert_eq!(
            result["content"].as_str().unwrap(),
            expected,
            "sidecar {path} content must round-trip"
        );
    }
}

#[tokio::test]
async fn update_skill_then_read_skill_reflects_new_body() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = registry_for(tmp.path());

    registry
        .get("create_skill")
        .unwrap()
        .execute_with_context(
            json!({
                "name": "mutable",
                "description": "v1",
                "body": "# Version 1\n"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let read = registry.get("read_skill").unwrap();
    let before = read
        .execute_with_context(json!({ "name": "mutable" }), &test_context())
        .await
        .unwrap();
    assert!(before["body"].as_str().unwrap().contains("Version 1"));
    assert_eq!(before["description"], "v1");

    registry
        .get("update_skill")
        .unwrap()
        .execute_with_context(
            json!({
                "name": "mutable",
                "description": "v2",
                "body": "# Version 2\n"
            }),
            &test_context(),
        )
        .await
        .unwrap();

    let after = read
        .execute_with_context(json!({ "name": "mutable" }), &test_context())
        .await
        .unwrap();
    assert!(
        after["body"].as_str().unwrap().contains("Version 2"),
        "read after update must reflect new body: {after}"
    );
    assert_eq!(after["description"], "v2");
}

#[tokio::test]
async fn delete_skill_then_read_skill_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = registry_for(tmp.path());

    for name in ["gone-soon", "still-here"] {
        registry
            .get("create_skill")
            .unwrap()
            .execute_with_context(
                json!({
                    "name": name,
                    "description": "t",
                    "body": "# body\n"
                }),
                &test_context(),
            )
            .await
            .unwrap();
    }
    registry
        .get("delete_skill")
        .unwrap()
        .execute_with_context(json!({ "name": "gone-soon" }), &test_context())
        .await
        .unwrap();

    let err = registry
        .get("read_skill")
        .unwrap()
        .execute_with_context(json!({ "name": "gone-soon" }), &test_context())
        .await
        .expect_err("deleted skill must not be readable");
    let msg = format!("{err}");
    assert!(
        msg.contains("'gone-soon'"),
        "error should name the skill: {msg}"
    );
    assert!(
        msg.contains("still-here"),
        "error hint should list remaining skills: {msg}"
    );
}
