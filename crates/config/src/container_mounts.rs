//! Declarative sandbox mounts and host path detection for nested containers.

use std::{
    collections::HashSet,
    fmt,
    path::{Component, Path, PathBuf},
    process::Command,
};

use {
    serde::{Deserialize, Serialize},
    tracing::{debug, warn},
};

use crate::schema::{HomePersistenceConfig, SandboxConfig};

pub const SANDBOX_HOME_DIR: &str = "/home/sandbox";
pub const CHELIX_CTL_GUEST_PATH: &str = "/usr/local/bin/chelix-ctl";

/// Access mode for a sandbox bind mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    Ro,
    Rw,
}

impl MountMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ro => "ro",
            Self::Rw => "rw",
        }
    }
}

impl fmt::Display for MountMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A host-to-guest bind mount consumed by every sandbox backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxMount {
    pub host: PathBuf,
    pub guest: PathBuf,
    pub mode: MountMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MountViolationKind {
    Host,
    Guest,
    Mode,
}

impl MountViolationKind {
    pub(crate) const fn field(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Guest => "guest",
            Self::Mode => "mode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountViolationReason {
    RelativeHost,
    RelativeGuest,
    DuplicateDataMount,
    InvalidDataMapping,
    ConfigExposure,
    ManagedHomeConflict,
    ManagedControlConflict,
}

impl MountViolationReason {
    const fn safe_description(self) -> &'static str {
        match self {
            Self::RelativeHost => "host path is not absolute",
            Self::RelativeGuest => "guest path is not absolute",
            Self::DuplicateDataMount => "duplicates the mandatory data_dir mount",
            Self::InvalidDataMapping => "conflicts with the mandatory data_dir mapping",
            Self::ConfigExposure => "source overlaps the protected config directory",
            Self::ManagedHomeConflict => "guest path conflicts with managed home persistence",
            Self::ManagedControlConflict => "guest path conflicts with managed chelix-ctl",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountViolation {
    pub kind: MountViolationKind,
    reason: MountViolationReason,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RejectedSandboxMount {
    pub index: usize,
    pub violation: MountViolation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxMountValidation {
    pub data_mount_exposes_config: bool,
    pub shared_home_exposes_config: bool,
    pub managed_guest_mounts_conflict: bool,
    pub rejected_custom_mounts: Vec<RejectedSandboxMount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SandboxMountAnalysis {
    mounts: Vec<SandboxMount>,
    validation: SandboxMountValidation,
}

#[derive(Debug)]
struct MountPlanContext {
    guest_data_dir: PathBuf,
    host_data_dir: PathBuf,
    config_dir: Option<PathBuf>,
    chelix_ctl: Option<PathBuf>,
}

impl MountPlanContext {
    fn active(cfg: &SandboxConfig) -> Self {
        let guest_data_dir = normalize_path(&crate::data_dir());
        let host_data_dir = effective_host_data_dir(cfg, &guest_data_dir);
        Self {
            guest_data_dir,
            host_data_dir,
            config_dir: crate::config_dir().map(|path| normalize_path(&path)),
            chelix_ctl: chelix_ctl_host_path(),
        }
    }
}

/// Build the complete declarative mount plan for a sandbox.
///
/// The `data_dir` mount is an unconditional read-write invariant. For session
/// home persistence the returned host path is the session *base* directory;
/// the runtime layer appends the sanitized sandbox identifier before creating
/// the bind mount.
#[must_use]
pub fn sandbox_mount_plan(cfg: &SandboxConfig) -> Vec<SandboxMount> {
    let analysis = sandbox_mount_analysis_with_context(cfg, &MountPlanContext::active(cfg));
    for rejected in &analysis.validation.rejected_custom_mounts {
        warn!(
            mount_index = rejected.index,
            field = rejected.violation.kind.field(),
            reason = rejected.violation.reason.safe_description(),
            "ignoring invalid custom sandbox mount"
        );
    }
    analysis.mounts
}

#[cfg(test)]
fn sandbox_mount_plan_with_context(
    cfg: &SandboxConfig,
    context: &MountPlanContext,
) -> Vec<SandboxMount> {
    sandbox_mount_analysis_with_context(cfg, context).mounts
}

fn sandbox_mount_analysis_with_context(
    cfg: &SandboxConfig,
    context: &MountPlanContext,
) -> SandboxMountAnalysis {
    let mut mounts = vec![SandboxMount {
        host: context.host_data_dir.clone(),
        guest: context.guest_data_dir.clone(),
        mode: MountMode::Rw,
    }];

    let managed_guest_conflict = managed_guest_mounts_conflict_with_context(cfg, context);
    let shared_home_exposes_config = shared_home_exposes_config_with_context(cfg, context);
    match cfg.home_persistence {
        HomePersistenceConfig::Off => {},
        HomePersistenceConfig::Session if !managed_guest_conflict => {
            mounts.push(SandboxMount {
                host: context
                    .host_data_dir
                    .join("sandbox")
                    .join("home")
                    .join("session"),
                guest: PathBuf::from(SANDBOX_HOME_DIR),
                mode: MountMode::Rw,
            });
        },
        HomePersistenceConfig::Shared if !managed_guest_conflict && !shared_home_exposes_config => {
            mounts.push(SandboxMount {
                host: shared_home_host_path(cfg, context),
                guest: PathBuf::from(SANDBOX_HOME_DIR),
                mode: MountMode::Rw,
            });
        },
        HomePersistenceConfig::Session | HomePersistenceConfig::Shared => {},
    }

    if let Some(host) = &context.chelix_ctl
        && !paths_overlap(&context.guest_data_dir, Path::new(CHELIX_CTL_GUEST_PATH))
    {
        mounts.push(SandboxMount {
            host: host.clone(),
            guest: PathBuf::from(CHELIX_CTL_GUEST_PATH),
            mode: MountMode::Ro,
        });
    }

    let mut rejected_custom_mounts = Vec::new();
    for (index, mount) in cfg.mounts.iter().enumerate() {
        if let Some(violation) = custom_mount_violation_with_context(cfg, mount, context) {
            rejected_custom_mounts.push(RejectedSandboxMount { index, violation });
        } else {
            mounts.push(mount.clone());
        }
    }

    SandboxMountAnalysis {
        mounts,
        validation: SandboxMountValidation {
            data_mount_exposes_config: data_mount_exposes_config_with_context(context),
            shared_home_exposes_config,
            managed_guest_mounts_conflict: managed_guest_conflict,
            rejected_custom_mounts,
        },
    }
}

fn configured_host_data_dir(cfg: &SandboxConfig, guest_data_dir: &Path) -> Option<PathBuf> {
    let configured = cfg
        .host_data_dir
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)?;
    let resolved = if configured.is_absolute() {
        configured
    } else {
        guest_data_dir.join(configured)
    };
    Some(normalize_path(&resolved))
}

#[must_use]
pub(crate) fn effective_host_data_dir(cfg: &SandboxConfig, guest_data_dir: &Path) -> PathBuf {
    configured_host_data_dir(cfg, guest_data_dir).unwrap_or_else(|| normalize_path(guest_data_dir))
}

fn shared_home_host_path(cfg: &SandboxConfig, context: &MountPlanContext) -> PathBuf {
    let guest_path = cfg
        .shared_home_dir
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                context.guest_data_dir.join(path)
            }
        })
        .unwrap_or_else(|| {
            context
                .guest_data_dir
                .join("sandbox")
                .join("home")
                .join("shared")
        });
    translate_data_path_to_host(&guest_path, context)
}

fn translate_data_path_to_host(path: &Path, context: &MountPlanContext) -> PathBuf {
    let normalized = normalize_path(path);
    let Ok(relative) = normalized.strip_prefix(&context.guest_data_dir) else {
        return normalized;
    };
    if relative.as_os_str().is_empty() {
        context.host_data_dir.clone()
    } else {
        context.host_data_dir.join(relative)
    }
}

fn chelix_ctl_host_path() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let ctl = executable.parent()?.join("chelix-ctl");
    ctl.is_file().then_some(ctl)
}

#[must_use]
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    path.components()
        .fold(PathBuf::new(), |mut normalized, component| {
            match component {
                Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
                Component::RootDir => normalized.push(component.as_os_str()),
                Component::CurDir => {},
                Component::ParentDir => {
                    normalized.pop();
                },
                Component::Normal(segment) => normalized.push(segment),
            }
            normalized
        })
}

#[must_use]
pub(crate) fn paths_overlap(first: &Path, second: &Path) -> bool {
    let first = normalize_path(first);
    let second = normalize_path(second);
    first.starts_with(&second) || second.starts_with(&first)
}

fn comparable_host_path(path: &Path) -> PathBuf {
    let normalized = normalize_path(path);
    normalized
        .canonicalize()
        .map_or(normalized, |canonical| normalize_path(&canonical))
}

fn host_paths_overlap(first: &Path, second: &Path) -> bool {
    let first = comparable_host_path(first);
    let second = comparable_host_path(second);
    first.starts_with(&second) || second.starts_with(&first)
}

fn data_mount_exposes_config_with_context(context: &MountPlanContext) -> bool {
    context.config_dir.as_ref().is_some_and(|config_dir| {
        paths_overlap(&context.guest_data_dir, config_dir)
            || host_paths_overlap(&context.host_data_dir, config_dir)
    })
}

#[must_use]
pub(crate) fn sandbox_mount_validation(cfg: &SandboxConfig) -> SandboxMountValidation {
    sandbox_mount_analysis_with_context(cfg, &MountPlanContext::active(cfg)).validation
}

fn shared_home_exposes_config_with_context(
    cfg: &SandboxConfig,
    context: &MountPlanContext,
) -> bool {
    cfg.home_persistence == HomePersistenceConfig::Shared
        && context.config_dir.as_ref().is_some_and(|config_dir| {
            host_paths_overlap(&shared_home_host_path(cfg, context), config_dir)
        })
}

fn managed_guest_mounts_conflict_with_context(
    cfg: &SandboxConfig,
    context: &MountPlanContext,
) -> bool {
    (cfg.home_persistence != HomePersistenceConfig::Off
        && paths_overlap(&context.guest_data_dir, Path::new(SANDBOX_HOME_DIR)))
        || (context.chelix_ctl.is_some()
            && paths_overlap(&context.guest_data_dir, Path::new(CHELIX_CTL_GUEST_PATH)))
}

fn custom_mount_violation_with_context(
    cfg: &SandboxConfig,
    mount: &SandboxMount,
    context: &MountPlanContext,
) -> Option<MountViolation> {
    if !mount.host.is_absolute() {
        return Some(MountViolation {
            kind: MountViolationKind::Host,
            reason: MountViolationReason::RelativeHost,
            message: format!(
                "sandbox mount host path must be absolute (got '{}')",
                mount.host.display()
            ),
        });
    }
    if !mount.guest.is_absolute() {
        return Some(MountViolation {
            kind: MountViolationKind::Guest,
            reason: MountViolationReason::RelativeGuest,
            message: format!(
                "sandbox mount guest path must be absolute (got '{}')",
                mount.guest.display()
            ),
        });
    }

    let host = normalize_path(&mount.host);
    let guest = normalize_path(&mount.guest);
    let is_exact_data_mount = host == context.host_data_dir
        && guest == context.guest_data_dir
        && mount.mode == MountMode::Rw;
    let data_host_overlap = host_paths_overlap(&host, &context.host_data_dir);
    let data_guest_overlap = paths_overlap(&guest, &context.guest_data_dir);
    if data_host_overlap || data_guest_overlap {
        if is_exact_data_mount {
            return Some(MountViolation {
                kind: MountViolationKind::Guest,
                reason: MountViolationReason::DuplicateDataMount,
                message: "sandbox data_dir mount is built in and must not be duplicated".into(),
            });
        }
        let kind = if host == context.host_data_dir && guest == context.guest_data_dir {
            MountViolationKind::Mode
        } else if data_host_overlap {
            MountViolationKind::Guest
        } else {
            MountViolationKind::Host
        };
        return Some(MountViolation {
            kind,
            reason: MountViolationReason::InvalidDataMapping,
            message: format!(
                "sandbox data_dir must map only from '{}' to the identical agent path '{}' in rw mode",
                context.host_data_dir.display(),
                context.guest_data_dir.display()
            ),
        });
    }

    if let Some(config_dir) = &context.config_dir
        && host_paths_overlap(&host, config_dir)
    {
        return Some(MountViolation {
            kind: MountViolationKind::Host,
            reason: MountViolationReason::ConfigExposure,
            message: format!(
                "sandbox mount source '{}' exposes the config directory '{}', which may contain credentials",
                mount.host.display(),
                config_dir.display()
            ),
        });
    }

    if cfg.home_persistence != HomePersistenceConfig::Off
        && paths_overlap(&guest, Path::new(SANDBOX_HOME_DIR))
    {
        return Some(MountViolation {
            kind: MountViolationKind::Guest,
            reason: MountViolationReason::ManagedHomeConflict,
            message: format!(
                "sandbox mount guest '{}' conflicts with the managed home-persistence mount",
                mount.guest.display()
            ),
        });
    }

    if context.chelix_ctl.is_some() && paths_overlap(&guest, Path::new(CHELIX_CTL_GUEST_PATH)) {
        return Some(MountViolation {
            kind: MountViolationKind::Guest,
            reason: MountViolationReason::ManagedControlConflict,
            message: format!(
                "sandbox mount guest '{}' conflicts with the managed chelix-ctl mount",
                mount.guest.display()
            ),
        });
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContainerMount {
    source: PathBuf,
    destination: PathBuf,
}

fn read_trimmed_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|contents| contents.trim().to_string())
        .filter(|contents| !contents.is_empty())
}

fn normalize_cgroup_container_ref(segment: &str) -> Option<String> {
    let mut value = segment.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(stripped) = value.strip_suffix(".scope") {
        value = stripped;
    }
    for prefix in ["docker-", "libpod-", "cri-containerd-"] {
        if let Some(stripped) = value.strip_prefix(prefix) {
            value = stripped;
            break;
        }
    }
    if value.len() < 12 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    Some(value.to_string())
}

pub fn current_container_references() -> Vec<String> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    for candidate in [
        std::env::var("HOSTNAME").ok(),
        read_trimmed_file("/etc/hostname"),
    ]
    .into_iter()
    .flatten()
    {
        if seen.insert(candidate.clone()) {
            refs.push(candidate);
        }
    }
    if let Ok(cgroup) = std::fs::read_to_string("/proc/self/cgroup") {
        for candidate in cgroup
            .lines()
            .flat_map(|line| line.split(['/', ':']))
            .filter_map(normalize_cgroup_container_ref)
        {
            if seen.insert(candidate.clone()) {
                refs.push(candidate);
            }
        }
    }
    refs
}

#[must_use]
fn parse_container_mounts_from_inspect(stdout: &str) -> Vec<ContainerMount> {
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(stdout) else {
        return Vec::new();
    };
    let root = json
        .as_array()
        .and_then(|entries| entries.first())
        .unwrap_or(&json);
    root.get("Mounts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let source = entry.get("Source")?.as_str()?.trim();
            let destination = entry.get("Destination")?.as_str()?.trim();
            if source.is_empty() || destination.is_empty() {
                return None;
            }
            Some(ContainerMount {
                source: PathBuf::from(source),
                destination: PathBuf::from(destination),
            })
        })
        .collect()
}

#[must_use]
fn resolve_host_path_from_mounts(guest_path: &Path, mounts: &[ContainerMount]) -> Option<PathBuf> {
    mounts
        .iter()
        .filter_map(|mount| {
            let relative = guest_path.strip_prefix(&mount.destination).ok()?;
            Some((
                mount.destination.components().count(),
                if relative.as_os_str().is_empty() {
                    mount.source.clone()
                } else {
                    mount.source.join(relative)
                },
            ))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, resolved)| resolved)
}

#[must_use]
fn inspect_container_mounts(cli: &str, reference: &str) -> Vec<ContainerMount> {
    let output = match Command::new(cli).args(["inspect", reference]).output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(
                cli,
                reference,
                stderr = %stderr.trim(),
                "container inspect failed while auto-detecting host data dir"
            );
            return Vec::new();
        },
        Err(error) => {
            debug!(
                cli,
                reference,
                %error,
                "could not inspect container while auto-detecting host data dir"
            );
            return Vec::new();
        },
    };
    parse_container_mounts_from_inspect(&String::from_utf8_lossy(&output.stdout))
}

#[must_use]
fn running_container_references(cli: &str) -> Vec<String> {
    let output = match Command::new(cli).args(["ps", "-q", "--no-trunc"]).output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(
                cli,
                stderr = %stderr.trim(),
                "container list failed while auto-detecting host data dir"
            );
            return Vec::new();
        },
        Err(error) => {
            debug!(
                cli,
                %error,
                "could not list containers while auto-detecting host data dir"
            );
            return Vec::new();
        },
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

#[must_use]
fn detect_host_data_dir_from_mount_sets<I>(guest_data_dir: &Path, mount_sets: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = Vec<ContainerMount>>,
{
    let mut detected: Option<PathBuf> = None;
    for mounts in mount_sets {
        if mounts.is_empty() {
            continue;
        }
        let Some(resolved) = resolve_host_path_from_mounts(guest_data_dir, &mounts) else {
            continue;
        };
        if let Some(existing) = &detected
            && existing != &resolved
        {
            debug!(
                guest_path = %guest_data_dir.display(),
                first_host_path = %existing.display(),
                other_host_path = %resolved.display(),
                "ambiguous host data dir from container mounts"
            );
            return None;
        }
        detected = Some(resolved);
    }
    detected
}

#[must_use]
pub fn detect_host_data_dir_with_references(
    cli: &str,
    guest_data_dir: &Path,
    references: &[String],
) -> Option<PathBuf> {
    let current_mount_sets = references
        .iter()
        .map(|reference| inspect_container_mounts(cli, reference));
    if let Some(resolved) = detect_host_data_dir_from_mount_sets(guest_data_dir, current_mount_sets)
    {
        debug!(
            cli,
            guest_path = %guest_data_dir.display(),
            host_path = %resolved.display(),
            "auto-detected host data dir from current container mounts"
        );
        return Some(resolved);
    }

    let running_mount_sets = running_container_references(cli)
        .into_iter()
        .map(|reference| inspect_container_mounts(cli, &reference));
    let resolved = detect_host_data_dir_from_mount_sets(guest_data_dir, running_mount_sets)?;
    debug!(
        cli,
        guest_path = %guest_data_dir.display(),
        host_path = %resolved.display(),
        "auto-detected host data dir by scanning running container mounts"
    );
    Some(resolved)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    fn test_context() -> MountPlanContext {
        MountPlanContext {
            guest_data_dir: PathBuf::from("/agent/.chelix"),
            host_data_dir: PathBuf::from("/host/chelix-data"),
            config_dir: Some(PathBuf::from("/agent/.config/chelix")),
            chelix_ctl: None,
        }
    }

    #[test]
    fn mount_mode_has_stable_container_value() {
        assert_eq!(MountMode::Ro.as_str(), "ro");
        assert_eq!(MountMode::Rw.to_string(), "rw");
    }

    #[test]
    fn mount_plan_always_includes_rw_data_dir() {
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Off,
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &test_context());

        assert_eq!(mounts, vec![SandboxMount {
            host: PathBuf::from("/host/chelix-data"),
            guest: PathBuf::from("/agent/.chelix"),
            mode: MountMode::Rw,
        }]);
    }

    #[test]
    fn mount_plan_maps_default_shared_home_through_host_data_dir() {
        let mounts = sandbox_mount_plan_with_context(&SandboxConfig::default(), &test_context());

        assert_eq!(mounts[1], SandboxMount {
            host: PathBuf::from("/host/chelix-data/sandbox/home/shared"),
            guest: PathBuf::from(SANDBOX_HOME_DIR),
            mode: MountMode::Rw,
        });
    }

    #[test]
    fn mount_plan_uses_session_home_base() {
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Session,
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &test_context());

        assert_eq!(
            mounts[1].host,
            Path::new("/host/chelix-data/sandbox/home/session")
        );
        assert_eq!(mounts[1].guest, Path::new(SANDBOX_HOME_DIR));
        assert_eq!(mounts[1].mode, MountMode::Rw);
    }

    #[test]
    fn mount_plan_maps_shared_home_inside_guest_data_dir_to_host() {
        let config = SandboxConfig {
            shared_home_dir: Some("/agent/.chelix/custom/home".into()),
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &test_context());

        assert_eq!(mounts[1].host, Path::new("/host/chelix-data/custom/home"));
    }

    #[test]
    fn mount_plan_includes_chelix_ctl_read_only_when_available() {
        let mut context = test_context();
        context.chelix_ctl = Some(PathBuf::from("/opt/chelix/bin/chelix-ctl"));
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Off,
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &context);

        assert_eq!(mounts[1], SandboxMount {
            host: PathBuf::from("/opt/chelix/bin/chelix-ctl"),
            guest: PathBuf::from(CHELIX_CTL_GUEST_PATH),
            mode: MountMode::Ro,
        });
    }

    #[test]
    fn mount_plan_appends_valid_custom_mount() {
        let custom = SandboxMount {
            host: PathBuf::from("/datasets/reference"),
            guest: PathBuf::from("/mnt/reference"),
            mode: MountMode::Ro,
        };
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Off,
            mounts: vec![custom.clone()],
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &test_context());

        assert_eq!(mounts.last(), Some(&custom));
    }

    #[test]
    fn mount_plan_reports_filtered_data_dir_alias_without_logging_paths() {
        let valid_mount = SandboxMount {
            host: PathBuf::from("/datasets/reference"),
            guest: PathBuf::from("/mnt/reference"),
            mode: MountMode::Ro,
        };
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Off,
            mounts: vec![valid_mount.clone(), SandboxMount {
                host: PathBuf::from("/host/chelix-data"),
                guest: PathBuf::from("/different/guest"),
                mode: MountMode::Rw,
            }],
            ..SandboxConfig::default()
        };

        let analysis = sandbox_mount_analysis_with_context(&config, &test_context());

        assert_eq!(analysis.mounts.len(), 2);
        assert_eq!(analysis.mounts.last(), Some(&valid_mount));
        let rejection = analysis
            .validation
            .rejected_custom_mounts
            .first()
            .expect("invalid custom mount must be reported");
        assert_eq!(rejection.index, 1);
        assert_eq!(rejection.violation.kind, MountViolationKind::Guest);
        assert_eq!(
            rejection.violation.reason.safe_description(),
            "conflicts with the mandatory data_dir mapping"
        );
        assert!(!rejection.violation.reason.safe_description().contains('/'));
    }

    #[test]
    fn mount_plan_filters_guest_ancestor_of_data_dir() {
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Off,
            mounts: vec![SandboxMount {
                host: PathBuf::from("/other/source"),
                guest: PathBuf::from("/agent"),
                mode: MountMode::Rw,
            }],
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &test_context());

        assert_eq!(mounts.len(), 1);
    }

    #[test]
    fn mount_plan_filters_config_dir_ancestor() {
        let mount = SandboxMount {
            host: PathBuf::from("/agent"),
            guest: PathBuf::from("/mnt/agent"),
            mode: MountMode::Ro,
        };
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Off,
            mounts: vec![mount.clone()],
            ..SandboxConfig::default()
        };

        let context = test_context();
        let violation = custom_mount_violation_with_context(&config, &mount, &context)
            .expect("config directory ancestor must be rejected");
        let mounts = sandbox_mount_plan_with_context(&config, &context);

        assert_eq!(violation.kind, MountViolationKind::Host);
        assert!(violation.message.contains("credentials"));
        assert_eq!(mounts.len(), 1);
    }

    #[test]
    fn mount_plan_filters_managed_shared_home_that_exposes_config_dir() {
        let config = SandboxConfig {
            shared_home_dir: Some("/agent".into()),
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &test_context());

        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].guest, Path::new("/agent/.chelix"));
    }

    #[test]
    fn mount_plan_does_not_overlay_data_dir_with_managed_home() {
        let mut context = test_context();
        context.guest_data_dir = PathBuf::from("/home/sandbox/data");

        let mounts = sandbox_mount_plan_with_context(&SandboxConfig::default(), &context);

        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].guest, Path::new("/home/sandbox/data"));
    }

    #[cfg(unix)]
    #[test]
    fn mount_plan_filters_symlink_to_config_dir() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config_dir = temp.path().join("config");
        std::fs::create_dir(&config_dir).expect("config dir");
        let link = temp.path().join("config-link");
        std::os::unix::fs::symlink(&config_dir, &link).expect("config symlink");
        let context = MountPlanContext {
            guest_data_dir: PathBuf::from("/agent/.chelix"),
            host_data_dir: temp.path().join("data"),
            config_dir: Some(config_dir),
            chelix_ctl: None,
        };
        let config = SandboxConfig {
            home_persistence: HomePersistenceConfig::Off,
            mounts: vec![SandboxMount {
                host: link,
                guest: PathBuf::from("/mnt/config"),
                mode: MountMode::Ro,
            }],
            ..SandboxConfig::default()
        };

        let mounts = sandbox_mount_plan_with_context(&config, &context);

        assert_eq!(mounts.len(), 1);
    }

    #[test]
    fn normalizes_parent_components_before_security_checks() {
        assert_eq!(
            normalize_path(Path::new("/safe/../agent/.config/chelix")),
            PathBuf::from("/agent/.config/chelix")
        );
        assert!(paths_overlap(
            Path::new("/agent/.config/chelix"),
            Path::new("/agent/.config/chelix/credentials.json")
        ));
    }

    #[test]
    fn normalizes_cgroup_container_ref() {
        assert_eq!(
            normalize_cgroup_container_ref(
                "docker-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.scope"
            ),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into())
        );
        assert_eq!(
            normalize_cgroup_container_ref(
                "libpod-abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdef.scope"
            ),
            Some("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdef".into())
        );
        assert!(normalize_cgroup_container_ref("user.slice").is_none());
    }

    #[test]
    fn parses_container_mounts_from_inspect() {
        let mounts = parse_container_mounts_from_inspect(
            r#"[{
            "Mounts": [
                {"Source": "/host/data", "Destination": "/home/chelix/.chelix"},
                {"Source": "/host/config", "Destination": "/home/chelix/.config/chelix"}
            ]
        }]"#,
        );
        assert_eq!(mounts, vec![
            ContainerMount {
                source: PathBuf::from("/host/data"),
                destination: PathBuf::from("/home/chelix/.chelix"),
            },
            ContainerMount {
                source: PathBuf::from("/host/config"),
                destination: PathBuf::from("/home/chelix/.config/chelix"),
            },
        ]);
    }

    #[test]
    fn resolves_host_path_from_mounts_prefers_longest_prefix() {
        let mounts = vec![
            ContainerMount {
                source: PathBuf::from("/host"),
                destination: PathBuf::from("/home"),
            },
            ContainerMount {
                source: PathBuf::from("/host/data"),
                destination: PathBuf::from("/home/chelix/.chelix"),
            },
        ];
        let resolved = resolve_host_path_from_mounts(
            &PathBuf::from("/home/chelix/.chelix/sandbox/home/shared"),
            &mounts,
        );
        assert_eq!(
            resolved,
            Some(PathBuf::from("/host/data/sandbox/home/shared"))
        );
    }

    #[test]
    fn detects_host_data_dir_from_mount_sets() {
        let guest_data_dir = PathBuf::from("/home/chelix/.chelix");
        let detected =
            detect_host_data_dir_from_mount_sets(&guest_data_dir, [vec![ContainerMount {
                source: PathBuf::from("/home/user/chelix/data"),
                destination: guest_data_dir.clone(),
            }]]);

        assert_eq!(detected, Some(PathBuf::from("/home/user/chelix/data")));
    }

    #[test]
    fn detects_ambiguous_mount_sets() {
        let guest_data_dir = PathBuf::from("/home/chelix/.chelix");
        let detected = detect_host_data_dir_from_mount_sets(&guest_data_dir, [
            vec![ContainerMount {
                source: PathBuf::from("/host/one"),
                destination: guest_data_dir.clone(),
            }],
            vec![ContainerMount {
                source: PathBuf::from("/host/two"),
                destination: guest_data_dir.clone(),
            }],
        ]);

        assert_eq!(detected, None);
    }
}
