#[cfg(feature = "bundled-skills")]
use crate::bundled::BundledSkillStore;
use {
    crate::{
        access::{discover_all, discover_skills, filter_skills_for_agent, visible_to_agent},
        skill_file::{self, SkillFields, SkillFiles, bus_error},
        skill_sidecars,
    },
    chelix_call_bus::*,
    chelix_config::{
        AgentsConfig,
        schema::{AgentSkillPolicy, SkillsConfig},
    },
    std::{collections::HashSet, path::PathBuf, sync::Arc},
    tokio::sync::RwLock,
};

type Result<T> = std::result::Result<T, SkillFileError>;

struct SkillHandler {
    files: Arc<SkillFiles>,
    #[cfg(feature = "bundled-skills")]
    bundled: Option<Arc<BundledSkillStore>>,
    agents: Arc<RwLock<AgentsConfig>>,
    skills: Arc<RwLock<SkillsConfig>>,
}

fn failed(error: impl std::fmt::Display) -> SkillFileError {
    tracing::error!(%error, "skill procedure failed");
    SkillFileError::Failed(error.to_string())
}

fn validate_name(name: &str) -> Result<()> {
    if crate::parse::validate_name(name) {
        return Ok(());
    }
    Err(failed(format!(
        "invalid skill name '{name}': must be 1-64 lowercase alphanumeric/hyphen chars"
    )))
}

impl SkillHandler {
    async fn policy(&self, id: &str) -> Result<AgentSkillPolicy> {
        self.agents
            .read()
            .await
            .get(id)
            .map(|agent| agent.skills.clone())
            .ok_or_else(|| failed(format!("unknown agent '{id}'")))
    }

    async fn actor_policy(&self, actor: &SkillActor) -> Result<Option<(String, AgentSkillPolicy)>> {
        match actor {
            SkillActor::Agent { id } => Ok(Some((id.clone(), self.policy(id).await?))),
            SkillActor::Process => Ok(None),
        }
    }

    fn personal_dir(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(self
            .files
            .root()
            .map_err(bus_error)?
            .join("skills")
            .join(name))
    }

    async fn existing(&self, path: &std::path::Path) -> Result<Option<SkillContent>> {
        match skill_file::read(&self.files, path, None).await {
            Ok(content) => Ok(Some(content)),
            Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            },
            Err(error) => Err(bus_error(error)),
        }
    }

    fn authorize(
        content: &SkillContent,
        policy: &Option<(String, AgentSkillPolicy)>,
        name: &str,
        create_or_delete: bool,
    ) -> Result<()> {
        if let Some((id, policy)) = policy
            && !visible_to_agent(id, &content.metadata, policy)
        {
            let message = if create_or_delete {
                format!("skill '{name}' not found")
            } else {
                format!("skill '{name}' does not exist; use create_skill first")
            };
            return Err(SkillFileError::Rejected(message));
        }
        Ok(())
    }

    async fn all(&self) -> Result<Vec<SkillMetadata>> {
        discover_all(
            &self.files,
            #[cfg(feature = "bundled-skills")]
            self.bundled.as_deref(),
        )
        .await
        .map_err(bus_error)
    }

    async fn visible(
        &self,
        id: &str,
        policy: &AgentSkillPolicy,
        config: &SkillsConfig,
    ) -> Result<Vec<SkillMetadata>> {
        let skills = discover_skills(
            &self.files,
            #[cfg(feature = "bundled-skills")]
            self.bundled.as_deref(),
            config,
        )
        .await
        .map_err(bus_error)?;
        Ok(filter_skills_for_agent(skills, id, policy))
    }

    async fn list(&self, request: ListSkills) -> Result<Vec<ListedSkill>> {
        let config = self.skills.read().await.clone();
        let skills = match request {
            ListSkills::Agent { id } => {
                self.visible(&id, &self.policy(&id).await?, &config).await?
            },
            ListSkills::Process {
                view: SkillView::Merged,
            } => {
                let all = self.all().await?;
                let mut seen: HashSet<String> = all
                    .iter()
                    .filter(|meta| meta.source != Some(SkillSource::Bundled))
                    .map(|meta| meta.name.clone())
                    .collect();
                all.into_iter()
                    .filter(|meta| {
                        meta.source != Some(SkillSource::Bundled) || seen.insert(meta.name.clone())
                    })
                    .collect()
            },
            ListSkills::Process {
                view: SkillView::Bundled,
            } => {
                #[cfg(feature = "bundled-skills")]
                {
                    self.store()?.discover().await.map_err(bus_error)?
                }
                #[cfg(not(feature = "bundled-skills"))]
                {
                    Vec::new()
                }
            },
        };
        Ok(skills
            .into_iter()
            .map(|metadata| {
                let bundled = metadata.source == Some(SkillSource::Bundled);
                let enabled = !bundled
                    || config
                        .is_bundled_skill_enabled(&metadata.name, metadata.category.as_deref());
                let category_enabled = !bundled
                    || metadata
                        .category
                        .as_ref()
                        .filter(|category| !category.is_empty())
                        .is_none_or(|category| {
                            !config.disabled_bundled_categories.contains(category)
                        });
                ListedSkill {
                    metadata,
                    enabled,
                    category_enabled,
                }
            })
            .collect())
    }

    async fn read(&self, request: ReadSkillFile) -> Result<SkillRead> {
        let policy = self.actor_policy(&request.actor).await?;
        let config = self.skills.read().await.clone();
        let meta = match request.locate {
            SkillLocate::PersonalDir | SkillLocate::ProjectDir => {
                validate_name(&request.name)?;
                let (subdir, source) = match request.locate {
                    SkillLocate::PersonalDir => ("skills", SkillSource::Personal),
                    _ => (".chelix/skills", SkillSource::Project),
                };
                let path = self
                    .files
                    .root()
                    .map_err(bus_error)?
                    .join(subdir)
                    .join(&request.name);
                let mut content =
                    skill_file::read(&self.files, &path.join("SKILL.md"), request.max_bytes)
                        .await
                        .map_err(bus_error)?;
                Self::authorize(&content, &policy, &request.name, false)?;
                content.metadata.source = Some(source);
                return self
                    .read_file(content, &request.name, request.file_path.as_deref())
                    .await;
            },
            SkillLocate::Discovered => {
                let skills = match &policy {
                    Some((id, policy)) => self.visible(id, policy, &config).await?,
                    None => self.all().await?,
                };
                let hint = if skills.is_empty() {
                    "no skills are currently available".into()
                } else {
                    format!(
                        "available skills: {}",
                        skills
                            .iter()
                            .map(|skill| skill.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                skills.into_iter().find(|meta| meta.name == request.name).ok_or_else(|| SkillFileError::Rejected(format!("skill '{}' not found ({hint}). Use one of the names listed in <available_skills>.", request.name)))?
            },
            SkillLocate::Bundled => {
                #[cfg(feature = "bundled-skills")]
                {
                    self.store()?
                        .discover()
                        .await
                        .map_err(bus_error)?
                        .into_iter()
                        .find(|meta| meta.name == request.name)
                        .ok_or_else(|| {
                            SkillFileError::NotFound(format!(
                                "bundled skill '{}' not found",
                                request.name
                            ))
                        })?
                }
                #[cfg(not(feature = "bundled-skills"))]
                {
                    return Err(failed("unsupported skill source 'bundled'"));
                }
            },
        };
        if meta.source == Some(SkillSource::Bundled) {
            #[cfg(feature = "bundled-skills")]
            {
                return self
                    .read_bundled(
                        meta,
                        request.file_path.as_deref(),
                        request.max_bytes,
                        &config,
                    )
                    .await;
            }
            #[cfg(not(feature = "bundled-skills"))]
            {
                return Err(failed("unsupported skill source 'bundled'"));
            }
        }
        let mut content =
            skill_file::read(&self.files, &meta.path.join("SKILL.md"), request.max_bytes)
                .await
                .map_err(bus_error)?;
        content.metadata.source = meta.source;
        self.read_file(content, &request.name, request.file_path.as_deref())
            .await
    }

    async fn read_file(
        &self,
        content: SkillContent,
        name: &str,
        file_path: Option<&str>,
    ) -> Result<SkillRead> {
        let relative = file_path
            .map(skill_sidecars::normalize_relative_skill_file_path)
            .transpose()
            .map_err(bus_error)?;
        let dir = tokio::fs::canonicalize(&content.metadata.path)
            .await
            .map_err(failed)?;
        let skill_dir_name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        let mut response = SkillRead {
            metadata: content.metadata,
            body: content.body,
            linked_files: Vec::new(),
            sidecar: None,
            skill_dir: None,
            skill_dir_name,
            enabled: true,
        };
        if let Some(relative) = relative {
            response.sidecar = Some(
                skill_sidecars::read_sidecar(name, &dir, &relative)
                    .await
                    .map_err(bus_error)?,
            );
        } else {
            response.linked_files = skill_sidecars::list_skill_sidecar_files(&dir)
                .await
                .map_err(bus_error)?;
        }
        Ok(response)
    }

    #[cfg(feature = "bundled-skills")]
    fn store(&self) -> Result<&BundledSkillStore> {
        self.bundled
            .as_deref()
            .ok_or_else(|| failed("bundled skill store is not installed"))
    }

    #[cfg(feature = "bundled-skills")]
    async fn read_bundled(
        &self,
        meta: SkillMetadata,
        file_path: Option<&str>,
        max_bytes: Option<u64>,
        config: &SkillsConfig,
    ) -> Result<SkillRead> {
        let store = self.store()?;
        let name = &meta.name;
        let enabled = config.is_bundled_skill_enabled(name, meta.category.as_deref());
        let body = store
            .read_skill(name)
            .await
            .ok_or_else(|| failed(format!("bundled skill '{name}' body not readable")))?;
        if let Some(max) = max_bytes
            && body.len() as u64 > max
        {
            return Err(failed(format!(
                "skill file exceeds maximum size of {max} bytes"
            )));
        }
        let mut linked_files = Vec::new();
        let mut sidecar = None;
        let mut skill_dir = None;
        if let Some(path) = file_path {
            let relative =
                skill_sidecars::normalize_relative_skill_file_path(path).map_err(bus_error)?;
            let path = relative.to_string_lossy().into_owned();
            let raw = match store.read_sidecar(name, &path).map_err(bus_error)? {
                Some(raw) => raw,
                None => {
                    let files = store.list_sidecars(name).map_err(bus_error)?;
                    let hint = if files.is_empty() {
                        "(none)".into()
                    } else {
                        files
                            .iter()
                            .map(|(path, _)| path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    return Err(SkillFileError::NotFound(format!(
                        "sidecar file '{path}' not found in bundled skill '{name}'. Available sidecar files: {hint}"
                    )));
                },
            };
            sidecar = Some(skill_sidecars::sidecar_body(path, raw.0).map_err(bus_error)?);
        } else {
            linked_files = store
                .list_sidecars(name)
                .map_err(bus_error)?
                .into_iter()
                .map(|(path, bytes)| SkillLinkedFile { path, bytes })
                .collect();
            if linked_files
                .iter()
                .any(|file| file.path.starts_with("scripts/"))
            {
                skill_dir = store.materialize_sidecars(name).map_err(bus_error)?;
            }
        }
        Ok(SkillRead {
            metadata: meta,
            body,
            linked_files,
            sidecar,
            skill_dir,
            skill_dir_name: None,
            enabled,
        })
    }

    async fn create(&self, request: CreateSkillFile) -> Result<SkillContent> {
        let dir = self.personal_dir(&request.name)?;
        let policy = self.actor_policy(&request.actor).await?;
        if let Some(content) = self.existing(&dir.join("SKILL.md")).await? {
            Self::authorize(&content, &policy, &request.name, true)?;
            return Err(SkillFileError::AlreadyExists(request.name));
        }
        tokio::fs::create_dir_all(&dir).await.map_err(failed)?;
        skill_file::create(
            &self.files,
            &dir.join("SKILL.md"),
            SkillFields {
                name: request.name,
                description: request.description,
                body: request.body,
            },
            request.creator_agent_id.as_deref(),
        )
        .await
        .map_err(bus_error)
    }

    async fn replace(&self, request: ReplaceSkillFile) -> Result<SkillContent> {
        let dir = self.personal_dir(&request.name)?;
        let policy = self.actor_policy(&request.actor).await?;
        if let Some(content) = self.existing(&dir.join("SKILL.md")).await? {
            Self::authorize(&content, &policy, &request.name, false)?;
        }
        match request.dir {
            ReplaceDir::Create => tokio::fs::create_dir_all(&dir).await.map_err(failed)?,
            ReplaceDir::RequireExisting => match tokio::fs::metadata(&dir).await {
                Ok(meta) if meta.is_dir() => {},
                Ok(_) => {
                    return Err(failed(format!(
                        "skill '{}' is not a directory",
                        request.name
                    )));
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SkillFileError::NotFound(format!(
                        "skill '{}' does not exist; use create_skill first",
                        request.name
                    )));
                },
                Err(error) => return Err(failed(error)),
            },
        }
        skill_file::replace(&self.files, &dir.join("SKILL.md"), SkillFields {
            name: request.name,
            description: request.description,
            body: request.body,
        })
        .await
        .map_err(bus_error)
    }

    async fn patch(&self, request: PatchSkillFile) -> Result<SkillContent> {
        let dir = self.personal_dir(&request.name)?;
        let policy = self.actor_policy(&request.actor).await?;
        if let Some(content) = self.existing(&dir.join("SKILL.md")).await? {
            Self::authorize(&content, &policy, &request.name, false)?;
        }
        skill_file::patch(
            &self.files,
            &dir.join("SKILL.md"),
            &request.patches,
            request.description.as_deref(),
        )
        .await
        .map_err(bus_error)
    }

    async fn delete(&self, request: DeleteSkillDirectory) -> Result<()> {
        let dir = self.personal_dir(&request.name)?;
        let policy = Some((
            request.agent_id.clone(),
            self.policy(&request.agent_id).await?,
        ));
        let meta = match tokio::fs::symlink_metadata(&dir).await {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SkillFileError::NotFound(format!(
                    "skill '{}' not found",
                    request.name
                )));
            },
            Err(error) => return Err(failed(error)),
        };
        if let Some(content) = self.existing(&dir.join("SKILL.md")).await? {
            Self::authorize(&content, &policy, &request.name, true)?;
        }
        if meta.is_symlink() {
            tokio::fs::remove_file(dir).await.map_err(failed)?;
        } else {
            tokio::fs::remove_dir_all(dir).await.map_err(failed)?;
        }
        Ok(())
    }

    async fn write_sidecars(&self, request: WriteSkillSidecars) -> Result<SkillDirectory> {
        let dir = self.personal_dir(&request.name)?;
        let policy = Some((
            request.agent_id.clone(),
            self.policy(&request.agent_id).await?,
        ));
        let content = self.existing(&dir.join("SKILL.md")).await?.ok_or_else(|| {
            SkillFileError::NotFound(format!(
                "skill '{}' does not exist; use create_skill first",
                request.name
            ))
        })?;
        Self::authorize(&content, &policy, &request.name, false)?;
        skill_sidecars::write_sidecars(
            self.files.root().map_err(bus_error)?,
            &request.name,
            &dir,
            request.files,
        )
        .await
        .map_err(bus_error)?;
        Ok(SkillDirectory { path: dir })
    }

    async fn toggle_skill(&self, request: SetBundledSkillEnabled) -> Result<BundledToggle> {
        #[cfg(feature = "bundled-skills")]
        {
            let skill = self
                .store()?
                .discover()
                .await
                .map_err(bus_error)?
                .into_iter()
                .find(|meta| meta.name == request.name)
                .ok_or_else(|| {
                    SkillFileError::NotFound(format!("bundled skill '{}' not found", request.name))
                })?;
            let mut config = self.skills.write().await;
            let apply = |skills: &mut SkillsConfig| {
                if request.enabled {
                    skills
                        .disabled_bundled_skills
                        .retain(|name| name != &request.name);
                    if let Some(category) = &skill.category {
                        skills
                            .disabled_bundled_categories
                            .retain(|name| name != category);
                    }
                } else if !skills.disabled_bundled_skills.contains(&request.name) {
                    skills.disabled_bundled_skills.push(request.name.clone());
                }
            };
            chelix_config::update_config(|cfg| apply(&mut cfg.skills))
                .map_err(|error| failed(format!("failed to save config: {error}")))?;
            apply(&mut config);
            Ok(BundledToggle {
                category: skill.category.clone(),
                enabled: config.is_bundled_skill_enabled(&request.name, skill.category.as_deref()),
            })
        }
        #[cfg(not(feature = "bundled-skills"))]
        {
            let _ = request;
            Err(failed("bundled skills are not available in this build"))
        }
    }

    async fn toggle_category(&self, request: SetBundledCategoryEnabled) -> Result<BundledToggle> {
        let mut config = self.skills.write().await;
        let apply = |skills: &mut SkillsConfig| {
            if request.enabled {
                skills
                    .disabled_bundled_categories
                    .retain(|category| category != &request.category);
            } else if !skills
                .disabled_bundled_categories
                .contains(&request.category)
            {
                skills
                    .disabled_bundled_categories
                    .push(request.category.clone());
            }
        };
        chelix_config::update_config(|cfg| apply(&mut cfg.skills))
            .map_err(|error| failed(format!("failed to save config: {error}")))?;
        apply(&mut config);
        Ok(BundledToggle {
            category: Some(request.category),
            enabled: request.enabled,
        })
    }
}

pub fn require_skill_files(bus: &CallBus) -> std::result::Result<(), RegError> {
    bus.require::<ReadSkillFile>()?;
    bus.require::<CreateSkillFile>()?;
    bus.require::<ReplaceSkillFile>()?;
    bus.require::<PatchSkillFile>()?;
    bus.require::<DeleteSkillDirectory>()?;
    bus.require::<WriteSkillSidecars>()?;
    bus.require::<ListSkills>()?;
    bus.require::<SetBundledSkillEnabled>()?;
    bus.require::<SetBundledCategoryEnabled>()
}

pub fn register_skill_files(
    bus: &CallBus,
    files: &Arc<SkillFiles>,
    #[cfg(feature = "bundled-skills")] bundled: Option<Arc<BundledSkillStore>>,
    root: PathBuf,
    agents: Arc<RwLock<AgentsConfig>>,
    skills: Arc<RwLock<SkillsConfig>>,
) -> std::result::Result<(), RegError> {
    files.root.set(root).map_err(|_| RegError::Duplicate)?;
    let handler = Arc::new(SkillHandler {
        files: Arc::clone(files),
        #[cfg(feature = "bundled-skills")]
        bundled,
        agents,
        skills,
    });
    macro_rules! register {
        ($request:ty, $method:ident) => {{
            let handler = Arc::clone(&handler);
            bus.register(move |request: $request| { let handler = Arc::clone(&handler); async move { handler.$method(request).await } })?;
        }};
    }
    register!(ReadSkillFile, read);
    register!(CreateSkillFile, create);
    register!(ReplaceSkillFile, replace);
    register!(PatchSkillFile, patch);
    register!(DeleteSkillDirectory, delete);
    register!(WriteSkillSidecars, write_sidecars);
    register!(ListSkills, list);
    register!(SetBundledSkillEnabled, toggle_skill);
    register!(SetBundledCategoryEnabled, toggle_category);
    Ok(())
}

pub fn bind_skill_files(
    bus: &CallBus,
    root: PathBuf,
    agents: Arc<RwLock<AgentsConfig>>,
    skills: Arc<RwLock<SkillsConfig>>,
) -> std::result::Result<(), String> {
    let files = SkillFiles::new()?;
    #[cfg(feature = "bundled-skills")]
    let bundled = Some(Arc::new(BundledSkillStore::new(Arc::clone(&files))));
    register_skill_files(
        bus,
        &files,
        #[cfg(feature = "bundled-skills")]
        bundled,
        root,
        agents,
        skills,
    )
    .map_err(|error| error.to_string())
}

pub fn open_skill_bus_at(root: &std::path::Path) -> std::result::Result<Arc<CallBus>, String> {
    let bus = CallBus::new();
    require_skill_files(&bus).map_err(|error| error.to_string())?;
    let files = SkillFiles::new()?;
    register_skill_files(
        &bus,
        &files,
        #[cfg(feature = "bundled-skills")]
        None,
        root.to_path_buf(),
        Arc::new(RwLock::new(AgentsConfig::default())),
        Arc::new(RwLock::new(SkillsConfig {
            enabled: true,
            ..Default::default()
        })),
    )
    .map_err(|error| error.to_string())?;
    bus.seal().map_err(|error| error.to_string())?;
    Ok(bus)
}

#[cfg(any(test, feature = "test-util"))]
pub fn open_skill_bus_with_agents(
    root: &std::path::Path,
    agents: Arc<RwLock<AgentsConfig>>,
    skills: Arc<RwLock<SkillsConfig>>,
) -> std::result::Result<Arc<CallBus>, String> {
    let bus = CallBus::new();
    require_skill_files(&bus).map_err(|error| error.to_string())?;
    let files = SkillFiles::new()?;
    register_skill_files(
        &bus,
        &files,
        #[cfg(feature = "bundled-skills")]
        None,
        root.to_path_buf(),
        agents,
        skills,
    )
    .map_err(|error| error.to_string())?;
    Ok(bus)
}

#[cfg(any(test, feature = "test-util"))]
pub fn open_skill_bus_for_agent(
    root: &std::path::Path,
    agent_id: &str,
    policy: AgentSkillPolicy,
) -> std::result::Result<Arc<CallBus>, String> {
    let mut agents = AgentsConfig::default();
    let mut agent = chelix_config::AgentConfig::new(
        agent_id,
        "test::model",
        chelix_config::schema::ReasoningEffort::from("off"),
    );
    agent.skills = policy;
    agents.entries.insert(agent_id.to_string(), agent);
    agents.default = agent_id.into();
    let bus = open_skill_bus_with_agents(
        root,
        Arc::new(RwLock::new(agents)),
        Arc::new(RwLock::new(SkillsConfig {
            enabled: true,
            ..Default::default()
        })),
    )?;
    bus.seal().map_err(|error| error.to_string())?;
    Ok(bus)
}

#[cfg(test)]
#[path = "skill_handlers_tests.rs"]
mod tests;
