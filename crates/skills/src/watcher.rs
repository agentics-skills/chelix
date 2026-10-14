//! Filesystem watcher for skill directories.
//!
//! Watches skill search paths for SKILL.md create/modify/delete events and sends
//! notifications through a channel so the gateway can broadcast `skills.changed`.

use std::{collections::BTreeMap, path::PathBuf};

use {
    notify_debouncer_full::{
        DebounceEventResult, Debouncer, RecommendedCache, new_debouncer, notify::RecursiveMode,
    },
    tokio::sync::mpsc,
    tracing::{debug, warn},
};

use crate::{discover::FsSkillDiscoverer, error::Result, types::SkillSource};

/// Events emitted by the skill watcher.
#[derive(Debug, Clone)]
pub enum SkillWatchEvent {
    /// A local skill's `SKILL.md` changed.
    SkillChanged,
}

/// A filesystem path plus the recursion mode to use when watching it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSpec {
    pub path: PathBuf,
    pub recursive_mode: RecursiveMode,
}

fn insert_watch_spec(
    specs: &mut BTreeMap<PathBuf, RecursiveMode>,
    path: PathBuf,
    recursive_mode: RecursiveMode,
) {
    specs
        .entry(path)
        .and_modify(|mode| {
            if recursive_mode == RecursiveMode::Recursive {
                *mode = RecursiveMode::Recursive;
            }
        })
        .or_insert(recursive_mode);
}

pub(crate) fn build_watch_specs(search_paths: &[(PathBuf, SkillSource)]) -> Vec<WatchSpec> {
    let mut specs = BTreeMap::<PathBuf, RecursiveMode>::new();

    for (path, source) in search_paths {
        match source {
            SkillSource::Project | SkillSource::Personal => {
                insert_watch_spec(&mut specs, path.clone(), RecursiveMode::Recursive);
            },
            SkillSource::Bundled => {},
        }
    }

    specs
        .into_iter()
        .map(|(path, recursive_mode)| WatchSpec {
            path,
            recursive_mode,
        })
        .collect()
}

pub fn default_watch_specs() -> Result<Vec<WatchSpec>> {
    let search_paths = FsSkillDiscoverer::default_paths();
    Ok(build_watch_specs(&search_paths))
}

/// Watches skill directories for SKILL.md changes with debouncing.
pub struct SkillWatcher {
    _debouncer: Debouncer<notify_debouncer_full::notify::RecommendedWatcher, RecommendedCache>,
}

impl SkillWatcher {
    /// Start watching the given directories. Returns the watcher and a receiver for events.
    ///
    /// The watcher must be kept alive (not dropped) for events to continue.
    pub fn start(
        specs: Vec<WatchSpec>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<SkillWatchEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();

        let debouncer = new_debouncer(
            std::time::Duration::from_millis(500),
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) => {
                    let mut skill_changed = false;
                    for event in events {
                        for path in &event.paths {
                            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                            if filename != "SKILL.md" {
                                continue;
                            }

                            use notify_debouncer_full::notify::EventKind;
                            match event.kind {
                                EventKind::Create(_)
                                | EventKind::Modify(_)
                                | EventKind::Remove(_) => {
                                    debug!(path = %path.display(), "skill watcher event");
                                    skill_changed = true;
                                },
                                _ => {},
                            }
                        }
                    }
                    if skill_changed {
                        let _ = tx.send(SkillWatchEvent::SkillChanged);
                    }
                },
                Err(errors) => {
                    for e in errors {
                        warn!(error = %e, "skill watcher error");
                    }
                },
            },
        )?;

        let mut watcher = Self {
            _debouncer: debouncer,
        };

        for spec in &specs {
            if spec.path.exists() {
                watcher._debouncer.watch(&spec.path, spec.recursive_mode)?;
                debug!(
                    path = %spec.path.display(),
                    recursive = matches!(spec.recursive_mode, RecursiveMode::Recursive),
                    "skill watcher: watching path"
                );
            }
        }

        Ok((watcher, rx))
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn has_spec(specs: &[WatchSpec], path: &Path, recursive_mode: RecursiveMode) -> bool {
        specs
            .iter()
            .any(|spec| spec.path == path && spec.recursive_mode == recursive_mode)
    }

    #[test]
    fn build_watch_specs_watches_project_and_personal_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let project_dir = data_dir.join(".chelix/skills");
        let personal_dir = data_dir.join("skills");
        let search_paths = vec![
            (project_dir.clone(), SkillSource::Project),
            (personal_dir.clone(), SkillSource::Personal),
        ];

        let specs = build_watch_specs(&search_paths);

        assert_eq!(specs.len(), 2);
        assert!(has_spec(&specs, &project_dir, RecursiveMode::Recursive));
        assert!(has_spec(&specs, &personal_dir, RecursiveMode::Recursive));
    }
}
