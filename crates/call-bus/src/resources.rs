//! Resource binding and file ownership checks.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::document::{Access, ResourceDecl, ResourceKind};

#[derive(Debug, Clone)]
pub struct BoundResource {
    pub id: String,
    pub kind: ResourceKind,
    pub access: Access,
    pub create: bool,
    pub file_mode: Option<u32>,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct ResourceSet {
    pub by_id: HashMap<String, BoundResource>,
}

impl ResourceSet {
    pub fn bind(&mut self, decl: &ResourceDecl, path: PathBuf) -> Result<(), String> {
        if self.by_id.contains_key(&decl.id) {
            return Err(format!("resource {} is already bound", decl.id));
        }
        if !path.is_absolute() {
            return Err(format!("bind path {} is not absolute", path.display()));
        }
        self.by_id.insert(
            decl.id.clone(),
            BoundResource {
                id: decl.id.clone(),
                kind: decl.kind.clone(),
                access: decl.access,
                create: decl.create,
                file_mode: decl.file_mode,
                path,
            },
        );
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&BoundResource, String> {
        self.by_id
            .get(id)
            .ok_or_else(|| format!("resource {id} is not bound"))
    }
}

pub fn paths_intersect(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

pub fn relative_file(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Ok(PathBuf::new());
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err("path must be relative".into());
    }
    if path.components().any(|component| matches!(component, Component::ParentDir)) {
        return Err("path must not contain ..".into());
    }
    Ok(path.to_path_buf())
}

pub fn mode_bits(mode: Option<u32>) -> Result<u32, String> {
    mode.ok_or_else(|| "file_mode is not declared".to_owned())
}
