#![allow(clippy::unwrap_used, clippy::expect_used)]
use {super::*, std::path::Path};

fn seed(root: &Path, source: &str, dir: &str, name: &str, access: &str, body: &str) -> PathBuf {
    let path = root.join(source).join(dir);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Test skill\n{access}\n---\n{body}"),
    )
    .unwrap();
    path
}
fn agent() -> SkillActor {
    SkillActor::Agent {
        id: "agent1".into(),
    }
}
fn bus(root: &Path) -> Arc<CallBus> {
    open_skill_bus_for_agent(root, "agent1", AgentSkillPolicy::default()).unwrap()
}
fn read_request(
    name: &str,
    actor: SkillActor,
    locate: SkillLocate,
    path: Option<&str>,
) -> ReadSkillFile {
    ReadSkillFile {
        name: name.into(),
        max_bytes: Some(MAX_SKILL_FILE_BYTES),
        actor,
        locate,
        file_path: path.map(str::to_owned),
    }
}
fn create_request(name: &str, actor: SkillActor) -> CreateSkillFile {
    CreateSkillFile {
        name: name.into(),
        description: "Test".into(),
        body: "created".into(),
        creator_agent_id: None,
        actor,
    }
}
fn replace_request(name: &str, actor: SkillActor, dir: ReplaceDir) -> ReplaceSkillFile {
    ReplaceSkillFile {
        name: name.into(),
        description: "Updated".into(),
        body: "updated".into(),
        actor,
        dir,
    }
}
fn patch_request(name: &str, actor: SkillActor) -> PatchSkillFile {
    PatchSkillFile {
        name: name.into(),
        patches: vec![("body".into(), "patched".into())],
        description: None,
        actor,
    }
}
fn writes(name: &str, paths: &[(&str, &str)]) -> WriteSkillSidecars {
    WriteSkillSidecars {
        name: name.into(),
        agent_id: "agent1".into(),
        files: paths
            .iter()
            .map(|(path, content)| SkillSidecarWrite {
                relative_path: (*path).into(),
                content: (*content).into(),
            })
            .collect(),
    }
}
fn error<T>(result: std::result::Result<T, CallError<SkillFileError>>) -> SkillFileError {
    match result {
        Err(CallError::Failed(error)) => error,
        Err(other) => panic!("unexpected bus error: {other}"),
        Ok(_) => panic!("expected procedure error"),
    }
}
fn failed_with<T>(result: std::result::Result<T, CallError<SkillFileError>>, text: &str) {
    match error(result) {
        SkillFileError::Failed(message) => assert!(message.contains(text), "{message}"),
        other => panic!("expected Failed: {other}"),
    }
}
fn rejected_with<T>(result: std::result::Result<T, CallError<SkillFileError>>, text: &str) {
    match error(result) {
        SkillFileError::Rejected(message) => assert!(message.contains(text), "{message}"),
        other => panic!("expected Rejected: {other}"),
    }
}

#[tokio::test]
async fn agent_discovery_filters_first_source_and_hint() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        ".chelix/skills",
        "demo",
        "demo",
        "denied_agents: [agent1]",
        "project",
    );
    seed(
        tmp.path(),
        "skills",
        "demo",
        "demo",
        "allowed_agents: [agent1]",
        "personal",
    );
    seed(tmp.path(), "skills", "visible", "visible", "", "body");
    let bus = bus(tmp.path());
    let listed = bus
        .call(ListSkills::Agent {
            id: "agent1".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|skill| skill.metadata.name.as_str())
            .collect::<Vec<_>>(),
        ["visible"]
    );
    let rejected = error(
        bus.call(read_request(
            "missing",
            agent(),
            SkillLocate::Discovered,
            None,
        ))
        .await,
    );
    assert!(matches!(rejected, SkillFileError::Rejected(_)));
    let text = rejected.to_string();
    assert!(text.contains("available skills: visible"));
    assert!(!text.contains("demo"));
    rejected_with(
        bus.call(read_request("demo", agent(), SkillLocate::Discovered, None))
            .await,
        "not found",
    );
    let listed = bus
        .call(ListSkills::Process {
            view: SkillView::Merged,
        })
        .await
        .unwrap();
    assert_eq!(listed[0].metadata.source, Some(SkillSource::Project));
    assert_eq!(
        listed
            .iter()
            .filter(|skill| skill.metadata.name == "demo")
            .count(),
        2
    );
    assert!(
        listed
            .iter()
            .all(|skill| skill.enabled && skill.category_enabled)
    );
}

#[tokio::test]
async fn unknown_agent_is_an_explicit_error() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = bus(tmp.path());
    failed_with(
        bus.call(ListSkills::Agent {
            id: "missing".into(),
        })
        .await,
        "unknown agent 'missing'",
    );
    failed_with(
        bus.call(read_request(
            "demo",
            SkillActor::Agent {
                id: "missing".into(),
            },
            SkillLocate::Discovered,
            None,
        ))
        .await,
        "unknown agent 'missing'",
    );
    failed_with(
        bus.call(DeleteSkillDirectory {
            name: "demo".into(),
            agent_id: "missing".into(),
        })
        .await,
        "unknown agent 'missing'",
    );
    failed_with(
        bus.call(WriteSkillSidecars {
            name: "demo".into(),
            agent_id: "missing".into(),
            files: Vec::new(),
        })
        .await,
        "unknown agent 'missing'",
    );
}

#[tokio::test]
async fn process_replace_directory_policy_and_empty_access_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = open_skill_bus_at(tmp.path()).unwrap();
    assert!(matches!(
        error(
            bus.call(replace_request(
                "demo",
                SkillActor::Process,
                ReplaceDir::RequireExisting
            ))
            .await
        ),
        SkillFileError::NotFound(_)
    ));
    assert!(!tmp.path().join("skills/demo").exists());
    let saved = bus
        .call(replace_request(
            "demo",
            SkillActor::Process,
            ReplaceDir::Create,
        ))
        .await
        .unwrap();
    assert_eq!(saved.body, "updated");
    assert!(saved.metadata.allowed_agents.is_empty());
    let created = bus
        .call(create_request("shared", SkillActor::Process))
        .await
        .unwrap();
    assert!(created.metadata.allowed_agents.is_empty());
}

#[tokio::test]
async fn mutations_validate_names_before_joining_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = bus(tmp.path());
    for locate in [SkillLocate::PersonalDir, SkillLocate::ProjectDir] {
        failed_with(
            bus.call(read_request("../x", SkillActor::Process, locate, None))
                .await,
            "invalid skill name '../x'",
        );
    }
    for name in ["..", ""] {
        failed_with(
            bus.call(create_request(name, agent())).await,
            "invalid skill name",
        );
        failed_with(
            bus.call(replace_request(name, agent(), ReplaceDir::Create))
                .await,
            "invalid skill name",
        );
        failed_with(
            bus.call(patch_request(name, agent())).await,
            "invalid skill name",
        );
        failed_with(
            bus.call(DeleteSkillDirectory {
                name: name.into(),
                agent_id: "agent1".into(),
            })
            .await,
            "invalid skill name",
        );
        failed_with(
            bus.call(writes(name, &[("file.txt", "text")])).await,
            "invalid skill name",
        );
        assert!(tmp.path().is_dir());
    }
}

#[tokio::test]
async fn personal_mutations_parse_frontmatter_before_refusal_and_write() {
    let tmp = tempfile::tempdir().unwrap();
    let path = seed(
        tmp.path(),
        "skills",
        "demo",
        "demo",
        "allowed_agents: invalid",
        "body",
    )
    .join("SKILL.md");
    let original = std::fs::read(&path).unwrap();
    let bus = bus(tmp.path());
    for actor in [agent(), SkillActor::Process] {
        failed_with(bus.call(create_request("demo", actor)).await, "frontmatter");
    }
    failed_with(
        bus.call(replace_request("demo", agent(), ReplaceDir::Create))
            .await,
        "frontmatter",
    );
    failed_with(
        bus.call(patch_request("demo", agent())).await,
        "frontmatter",
    );
    failed_with(
        bus.call(DeleteSkillDirectory {
            name: "demo".into(),
            agent_id: "agent1".into(),
        })
        .await,
        "frontmatter",
    );
    failed_with(
        bus.call(writes("demo", &[("file.txt", "changed")])).await,
        "frontmatter",
    );
    failed_with(
        bus.call(read_request(
            "demo",
            SkillActor::Process,
            SkillLocate::PersonalDir,
            None,
        ))
        .await,
        "frontmatter",
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn personal_mutations_reject_invisible_files() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = bus(tmp.path());
    for access in ["allowed_agents: [other]", "denied_agents: [agent1]"] {
        let dir = seed(tmp.path(), "skills", "demo", "demo", access, "body");
        let original = std::fs::read(dir.join("SKILL.md")).unwrap();
        rejected_with(
            bus.call(create_request("demo", agent())).await,
            "skill 'demo' not found",
        );
        rejected_with(
            bus.call(replace_request(
                "demo",
                agent(),
                ReplaceDir::RequireExisting,
            ))
            .await,
            "skill 'demo' does not exist; use create_skill first",
        );
        rejected_with(
            bus.call(patch_request("demo", agent())).await,
            "skill 'demo' does not exist; use create_skill first",
        );
        rejected_with(
            bus.call(DeleteSkillDirectory {
                name: "demo".into(),
                agent_id: "agent1".into(),
            })
            .await,
            "skill 'demo' not found",
        );
        rejected_with(
            bus.call(writes("demo", &[("file.txt", "changed")])).await,
            "skill 'demo' does not exist; use create_skill first",
        );
        rejected_with(
            bus.call(read_request(
                "demo",
                agent(),
                SkillLocate::PersonalDir,
                None,
            ))
            .await,
            "does not exist",
        );
        assert_eq!(std::fs::read(dir.join("SKILL.md")).unwrap(), original);
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
    }
}

#[tokio::test]
async fn process_discovery_uses_registered_root_and_latest_content() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = open_skill_bus_at(tmp.path()).unwrap();
    rejected_with(
        bus.call(read_request(
            "demo",
            SkillActor::Process,
            SkillLocate::Discovered,
            None,
        ))
        .await,
        "no skills are currently available",
    );
    seed(tmp.path(), ".chelix/skills", "demo", "demo", "", "project");
    seed(tmp.path(), "skills", "demo", "demo", "", "personal");
    seed(tmp.path(), "skills", "other", "other", "", "other body");
    let request = || read_request("demo", SkillActor::Process, SkillLocate::Discovered, None);
    let content = bus.call(request()).await.unwrap();
    assert_eq!(content.body, "project");
    assert_eq!(content.metadata.source, Some(SkillSource::Project));
    assert_eq!(content.skill_dir_name.as_deref(), Some("demo"));
    assert_eq!(
        bus.call(read_request(
            "other",
            SkillActor::Process,
            SkillLocate::Discovered,
            None
        ))
        .await
        .unwrap()
        .body,
        "other body"
    );
    seed(
        tmp.path(),
        ".chelix/skills",
        "demo",
        "demo",
        "",
        "new project body",
    );
    assert_eq!(bus.call(request()).await.unwrap().body, "new project body");
}

#[tokio::test]
async fn sidecar_validation_precedes_writes_and_success_is_audited() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed(tmp.path(), "skills", "demo", "demo", "", "body");
    let bus = bus(tmp.path());
    let oversized = "x".repeat(MAX_SIDECAR_FILE_BYTES + 1);
    let cases = [
        (vec![], "at least one file"),
        (vec![("../escape", "x")], "path traversal"),
        (vec![(".hidden/file", "x")], "hidden path"),
        (vec![("/absolute", "x")], "must be relative"),
        (vec![("SKILL.md", "x")], "SKILL.md must be managed"),
        (vec![("a", "x"), ("a", "y")], "duplicate file path"),
        (vec![("a", oversized.as_str())], "exceeds maximum size"),
    ];
    for (paths, text) in cases {
        failed_with(bus.call(writes("demo", &paths)).await, text);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }
    let content = "x".repeat(MAX_SIDECAR_FILE_BYTES);
    let files = (0..5)
        .map(|index| SkillSidecarWrite {
            relative_path: format!("file{index}"),
            content: content.clone(),
        })
        .collect();
    failed_with(
        bus.call(WriteSkillSidecars {
            name: "demo".into(),
            agent_id: "agent1".into(),
            files,
        })
        .await,
        "total file content",
    );
    let files = (0..=MAX_SIDECAR_FILES_PER_CALL)
        .map(|index| SkillSidecarWrite {
            relative_path: format!("file{index}"),
            content: String::new(),
        })
        .collect();
    failed_with(
        bus.call(WriteSkillSidecars {
            name: "demo".into(),
            agent_id: "agent1".into(),
            files,
        })
        .await,
        "too many files",
    );
    let directory = bus
        .call(writes("demo", &[("references/api.md", "text")]))
        .await
        .unwrap();
    assert_eq!(directory.path, dir);
    let line = std::fs::read_to_string(tmp.path().join("logs/security-audit.jsonl")).unwrap();
    let audit: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(audit["event"], "skills.sidecar_files.write");
    assert!(audit["ts"].as_u64().is_some());
    assert_eq!(audit["details"]["files"][0]["path"], "references/api.md");
}

#[tokio::test]
async fn sidecar_writes_preserve_completed_files_on_later_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed(tmp.path(), "skills", "demo", "demo", "", "body");
    std::fs::create_dir(dir.join("collision")).unwrap();
    let bus = bus(tmp.path());
    failed_with(
        bus.call(writes("demo", &[
            ("first.txt", "ok"),
            ("collision", "failure"),
        ]))
        .await,
        "target 'collision' is a directory",
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("first.txt")).unwrap(),
        "ok"
    );
}

#[tokio::test]
async fn sidecar_listing_is_sorted_capped_per_subdirectory_and_typed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed(tmp.path(), "skills", "demo", "demo", "", "body");
    for sub in crate::SIDECAR_SUBDIRS {
        std::fs::create_dir_all(dir.join(sub).join("nested")).unwrap();
        std::fs::write(dir.join(sub).join(".hidden"), "hidden").unwrap();
        for index in 0..MAX_SIDECAR_FILES_PER_SUBDIR + 2 {
            std::fs::write(dir.join(sub).join(format!("{index:02}.txt")), "text").unwrap();
        }
    }
    let bus = bus(tmp.path());
    let content = bus
        .call(read_request("demo", agent(), SkillLocate::Discovered, None))
        .await
        .unwrap();
    assert_eq!(content.linked_files.len(), 4 * MAX_SIDECAR_FILES_PER_SUBDIR);
    assert!(
        content
            .linked_files
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
    );
    for sub in crate::SIDECAR_SUBDIRS {
        assert_eq!(
            content
                .linked_files
                .iter()
                .filter(|file| file.path.starts_with(&format!("{sub}/")))
                .count(),
            MAX_SIDECAR_FILES_PER_SUBDIR
        );
    }
    assert!(content.linked_files.iter().all(|file| file.bytes == 4
        && !file.path.contains("hidden")
        && !file.path.contains("nested")));
    std::fs::write(dir.join("assets/binary.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let binary = bus
        .call(read_request(
            "demo",
            agent(),
            SkillLocate::Discovered,
            Some("assets/binary.bin"),
        ))
        .await
        .unwrap()
        .sidecar
        .unwrap();
    assert!(binary.is_binary);
    assert!(binary.text.is_none());
    assert_eq!(binary.bytes, 3);
    let exact = vec![b'x'; MAX_SIDECAR_FILE_BYTES];
    std::fs::write(dir.join("assets/exact.txt"), &exact).unwrap();
    let sidecar = bus
        .call(read_request(
            "demo",
            agent(),
            SkillLocate::Discovered,
            Some("assets/exact.txt"),
        ))
        .await
        .unwrap()
        .sidecar
        .unwrap();
    assert_eq!(sidecar.bytes, MAX_SIDECAR_FILE_BYTES as u64);
    assert_eq!(sidecar.text.unwrap().len(), exact.len());
}

#[tokio::test]
async fn primary_read_lists_resources_and_missing_sidecar_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed(tmp.path(), "skills", "demo", "demo", "", "body");
    for (path, text) in [
        ("references/api.md", "api"),
        ("references/guide.md", "guide"),
        ("templates/prompt.txt", "prompt"),
        ("scripts/run.sh", "script"),
        ("assets/data.txt", "asset"),
        ("references/nested/file.txt", "nested"),
    ] {
        let target = dir.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, text).unwrap();
    }
    let bus = bus(tmp.path());
    let content = bus
        .call(read_request("demo", agent(), SkillLocate::Discovered, None))
        .await
        .unwrap();
    let paths = content
        .linked_files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, [
        "assets/data.txt",
        "references/api.md",
        "references/guide.md",
        "scripts/run.sh",
        "templates/prompt.txt"
    ]);
    let missing = error(
        bus.call(read_request(
            "demo",
            agent(),
            SkillLocate::Discovered,
            Some("references/does-not-exist.md"),
        ))
        .await,
    )
    .to_string();
    for path in [
        "references/does-not-exist.md",
        "references/api.md",
        "references/guide.md",
    ] {
        assert!(missing.contains(path));
    }
    let nested = bus
        .call(read_request(
            "demo",
            agent(),
            SkillLocate::Discovered,
            Some("references/nested/file.txt"),
        ))
        .await
        .unwrap()
        .sidecar
        .unwrap();
    assert_eq!(nested.text.as_deref(), Some("nested"));
}

#[tokio::test]
async fn read_limits_and_relative_paths_are_enforced_by_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed(tmp.path(), "skills", "demo", "demo", "", "body");
    let bus = bus(tmp.path());
    for (path, message) in [
        ("../secret.txt", "path traversal is not allowed"),
        ("/etc/passwd", "file path must be relative"),
        (
            "SKILL.md",
            "SKILL.md must be managed with create_skill/update_skill",
        ),
        ("   ", "file path must not be empty"),
    ] {
        failed_with(
            bus.call(read_request(
                "demo",
                agent(),
                SkillLocate::Discovered,
                Some(path),
            ))
            .await,
            message,
        );
    }
    std::fs::write(dir.join("large.txt"), vec![
        b'x';
        MAX_SIDECAR_FILE_BYTES + 1
    ])
    .unwrap();
    failed_with(
        bus.call(read_request(
            "demo",
            agent(),
            SkillLocate::Discovered,
            Some("large.txt"),
        ))
        .await,
        "exceeds maximum size",
    );
    seed(
        tmp.path(),
        "skills",
        "demo",
        "demo",
        "",
        &"x".repeat(MAX_SKILL_FILE_BYTES as usize + 1),
    );
    failed_with(
        bus.call(read_request("demo", agent(), SkillLocate::Discovered, None))
            .await,
        "exceeds maximum size",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn linked_skill_directory_supports_read_patch_sidecar_write_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let target = seed(tmp.path(), "target", "demo", "demo", "", "body");
    std::fs::create_dir_all(tmp.path().join("skills")).unwrap();
    let link = tmp.path().join("skills/demo");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let bus = bus(tmp.path());
    assert_eq!(
        bus.call(read_request("demo", agent(), SkillLocate::Discovered, None))
            .await
            .unwrap()
            .body,
        "body"
    );
    bus.call(patch_request("demo", agent())).await.unwrap();
    assert!(
        std::fs::read_to_string(target.join("SKILL.md"))
            .unwrap()
            .ends_with("patched")
    );
    bus.call(writes("demo", &[("script.sh", "echo test")]))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(target.join("script.sh")).unwrap(),
        "echo test"
    );
    let sidecar = bus
        .call(read_request(
            "demo",
            agent(),
            SkillLocate::Discovered,
            Some("script.sh"),
        ))
        .await
        .unwrap()
        .sidecar
        .unwrap();
    assert_eq!(sidecar.text.as_deref(), Some("echo test"));
    bus.call(DeleteSkillDirectory {
        name: "demo".into(),
        agent_id: "agent1".into(),
    })
    .await
    .unwrap();
    assert!(!link.exists());
    assert!(target.join("SKILL.md").is_file());
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_symlinks_read_and_write_the_target() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed(tmp.path(), "skills", "demo", "demo", "", "body");
    std::fs::create_dir(dir.join("references")).unwrap();
    let target = tmp.path().join("external.txt");
    std::fs::write(&target, "reference").unwrap();
    let link = dir.join("references/api.md");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let bus = bus(tmp.path());
    let content = bus
        .call(read_request(
            "demo",
            agent(),
            SkillLocate::Discovered,
            Some("references/api.md"),
        ))
        .await
        .unwrap();
    assert_eq!(content.sidecar.unwrap().text.as_deref(), Some("reference"));
    let content = bus
        .call(read_request("demo", agent(), SkillLocate::Discovered, None))
        .await
        .unwrap();
    assert_eq!(content.linked_files[0].bytes, 9);
    bus.call(writes("demo", &[("references/api.md", "updated")]))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(target).unwrap(), "updated");
    assert!(std::fs::symlink_metadata(link).unwrap().is_symlink());
}

#[tokio::test]
async fn delete_handles_missing_empty_and_visible_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = bus(tmp.path());
    assert!(matches!(
        error(
            bus.call(DeleteSkillDirectory {
                name: "missing".into(),
                agent_id: "agent1".into()
            })
            .await
        ),
        SkillFileError::NotFound(_)
    ));
    std::fs::create_dir_all(tmp.path().join("skills/empty")).unwrap();
    bus.call(DeleteSkillDirectory {
        name: "empty".into(),
        agent_id: "agent1".into(),
    })
    .await
    .unwrap();
    let dir = seed(
        tmp.path(),
        "skills",
        "visible",
        "visible",
        "allowed_agents: [agent1]",
        "body",
    );
    bus.call(DeleteSkillDirectory {
        name: "visible".into(),
        agent_id: "agent1".into(),
    })
    .await
    .unwrap();
    assert!(!dir.exists());
}

#[cfg(feature = "bundled-skills")]
fn bundled_bus(root: &Path) -> Arc<CallBus> {
    let files = SkillFiles::new().unwrap();
    let store = Arc::new(BundledSkillStore::with_materialize_dir(
        root.join("materialized"),
        Arc::clone(&files),
    ));
    let mut agents = AgentsConfig::default();
    agents.entries.insert(
        "agent1".into(),
        chelix_config::AgentConfig::new(
            "agent1",
            "test::model",
            chelix_config::schema::ReasoningEffort::from("off"),
        ),
    );
    let bus = CallBus::new();
    require_skill_files(&bus).unwrap();
    register_skill_files(
        &bus,
        &files,
        Some(store),
        root.to_path_buf(),
        Arc::new(RwLock::new(agents)),
        Arc::new(RwLock::new(SkillsConfig::default())),
    )
    .unwrap();
    bus.seal().unwrap();
    bus
}

#[cfg(feature = "bundled-skills")]
#[tokio::test]
async fn bundled_source_is_explicit_and_requires_installed_store() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        "skills",
        "no-such-bundled",
        "no-such-bundled",
        "",
        "personal",
    );
    let bus = bundled_bus(tmp.path());
    assert!(
        matches!(error(bus.call(read_request("no-such-bundled", SkillActor::Process, SkillLocate::Bundled, None)).await), SkillFileError::NotFound(message) if message == "bundled skill 'no-such-bundled' not found")
    );
    let bus = open_skill_bus_at(tmp.path()).unwrap();
    failed_with(
        bus.call(read_request(
            "mcp-servers",
            SkillActor::Process,
            SkillLocate::Bundled,
            None,
        ))
        .await,
        "bundled skill store is not installed",
    );
    failed_with(
        bus.call(ListSkills::Process {
            view: SkillView::Bundled,
        })
        .await,
        "bundled skill store is not installed",
    );
    failed_with(
        bus.call(SetBundledSkillEnabled {
            name: "mcp-servers".into(),
            enabled: false,
        })
        .await,
        "bundled skill store is not installed",
    );
}

#[cfg(feature = "bundled-skills")]
#[tokio::test]
async fn bundled_toggles_update_persisted_and_live_state_after_success() {
    struct ConfigGuard;
    impl Drop for ConfigGuard {
        fn drop(&mut self) {
            chelix_config::clear_config_dir();
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    chelix_config::set_config_dir(tmp.path().to_path_buf());
    let _guard = ConfigGuard;
    chelix_config::initialize_config().unwrap();
    let path = tmp.path().join("chelix.toml");
    let original = std::fs::read(&path).unwrap();
    let bus = bundled_bus(tmp.path());
    assert!(
        matches!(error(bus.call(SetBundledSkillEnabled { name: "no-such-bundled".into(), enabled: false }).await), SkillFileError::NotFound(message) if message == "bundled skill 'no-such-bundled' not found")
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let read = || {
        read_request(
            "mcp-servers",
            SkillActor::Process,
            SkillLocate::Bundled,
            None,
        )
    };
    let discovered = bus
        .call(read_request(
            "mcp-servers",
            SkillActor::Process,
            SkillLocate::Discovered,
            None,
        ))
        .await
        .unwrap();
    assert!(!discovered.body.is_empty());
    bus.call(SetBundledSkillEnabled {
        name: "mcp-servers".into(),
        enabled: false,
    })
    .await
    .unwrap();
    assert!(
        chelix_config::discover_and_load()
            .unwrap()
            .skills
            .disabled_bundled_skills
            .contains(&"mcp-servers".to_string())
    );
    assert!(
        bus.call(ListSkills::Agent {
            id: "agent1".into()
        })
        .await
        .unwrap()
        .is_empty()
    );
    let listed = bus
        .call(ListSkills::Process {
            view: SkillView::Bundled,
        })
        .await
        .unwrap();
    assert!(!listed[0].enabled);
    assert!(listed[0].category_enabled);
    let content = bus.call(read()).await.unwrap();
    assert!(!content.enabled);
    assert!(!content.body.is_empty());
    bus.call(SetBundledSkillEnabled {
        name: "mcp-servers".into(),
        enabled: true,
    })
    .await
    .unwrap();
    bus.call(SetBundledCategoryEnabled {
        category: "devops".into(),
        enabled: false,
    })
    .await
    .unwrap();
    let listed = bus
        .call(ListSkills::Process {
            view: SkillView::Bundled,
        })
        .await
        .unwrap();
    assert!(!listed[0].enabled);
    assert!(!listed[0].category_enabled);
    bus.call(SetBundledSkillEnabled {
        name: "mcp-servers".into(),
        enabled: true,
    })
    .await
    .unwrap();
    assert!(bus.call(read()).await.unwrap().enabled);
    let saved = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("removed_option = true\n{saved}")).unwrap();
    failed_with(
        bus.call(SetBundledSkillEnabled {
            name: "mcp-servers".into(),
            enabled: false,
        })
        .await,
        "failed to save config",
    );
    assert!(
        bus.call(ListSkills::Agent {
            id: "agent1".into()
        })
        .await
        .unwrap()
        .iter()
        .any(|skill| skill.metadata.name == "mcp-servers")
    );
}
