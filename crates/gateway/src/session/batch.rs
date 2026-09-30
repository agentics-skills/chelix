use std::collections::HashSet;

use super::{service::is_archivable_entry, *};

pub(super) struct SessionBatch<'a> {
    service: &'a LiveSessionService,
}

impl<'a> SessionBatch<'a> {
    pub(super) fn new(service: &'a LiveSessionService) -> Self {
        Self { service }
    }

    pub(super) async fn delete(&self, params: Value) -> ServiceResult {
        let Some(key) = params.get("key").and_then(Value::as_str) else {
            return self.service.delete_impl(params).await;
        };
        if key == "main" {
            return self.service.delete_impl(params).await;
        }

        let force = params
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let order = self.keys_leaf_to_root(key).await?;
        for session_key in &order {
            if session_key == "main" {
                return Err("cannot delete the main session".into());
            }
            let entry = self
                .service
                .metadata
                .get(session_key)
                .await
                .map_err(ServiceError::message)?
                .ok_or_else(|| {
                    ServiceError::message(format!("session '{session_key}' not found"))
                })?;
            self.service.preflight_session_delete(&entry, force).await?;
        }

        let mut cleanup_errors = Vec::new();
        for session_key in &order {
            if let Err(error) = self
                .service
                .delete_impl(serde_json::json!({
                    "key": session_key,
                    "force": force,
                }))
                .await
            {
                let still_present = match self.service.metadata.get(session_key).await {
                    Ok(entry) => entry,
                    Err(get_error) => {
                        return Err(error_with_cleanup(
                            &cleanup_errors,
                            ServiceError::message(get_error),
                        ));
                    },
                };
                if still_present.is_none() {
                    cleanup_errors.push(error.to_string());
                    continue;
                }
                return Err(error_with_cleanup(&cleanup_errors, error));
            }
        }
        if cleanup_errors.is_empty() {
            Ok(serde_json::json!({ "ok": true }))
        } else {
            Err(ServiceError::message(cleanup_errors.join("; ")))
        }
    }

    pub(super) async fn archive(&self, params: Value) -> ServiceResult {
        let Some(key) = params.get("key").and_then(Value::as_str) else {
            return self.service.patch_one(params).await;
        };

        let order = self.keys_leaf_to_root(key).await?;
        let mut descendant_keys = Vec::new();
        for session_key in &order {
            let entry = self
                .service
                .metadata
                .get(session_key)
                .await
                .map_err(ServiceError::message)?
                .ok_or_else(|| format!("session '{session_key}' not found"))?;
            let archivable = is_archivable_entry(&self.service.metadata, &entry).await?;
            if session_key != key {
                if entry.archived || !archivable {
                    continue;
                }
                descendant_keys.push(session_key.clone());
                continue;
            }
            if !archivable {
                return Err(ServiceError::message(format!(
                    "session '{session_key}' cannot be archived"
                )));
            }
        }

        let result = self.service.patch_one(params).await?;
        for session_key in descendant_keys {
            self.service
                .metadata
                .patch_session(
                    &session_key,
                    chelix_sessions::metadata::SessionMetadataPatch {
                        archived: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(ServiceError::message)?;
        }
        Ok(result)
    }

    async fn keys_leaf_to_root(&self, root_key: &str) -> Result<Vec<String>, ServiceError> {
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root_key.to_string()];

        while let Some(key) = stack.pop() {
            if !seen.insert(key.clone()) {
                continue;
            }
            order.push(key.clone());
            for child in self
                .service
                .metadata
                .list_children(&key)
                .await
                .map_err(ServiceError::message)?
            {
                stack.push(child.key);
            }
        }

        order.reverse();
        Ok(order)
    }
}

fn error_with_cleanup(cleanup_errors: &[String], error: ServiceError) -> ServiceError {
    if cleanup_errors.is_empty() {
        return error;
    }
    let mut parts = cleanup_errors.to_vec();
    parts.push(error.to_string());
    ServiceError::message(parts.join("; "))
}
