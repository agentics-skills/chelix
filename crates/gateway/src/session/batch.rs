use std::collections::HashSet;

use super::*;

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
        let Some(object) = params.as_object() else {
            return Err(ServiceError::message(
                "archive accepts only key and archived",
            ));
        };
        if object.len() != 2
            || !object.contains_key("key")
            || object.get("archived").and_then(Value::as_bool) != Some(true)
        {
            return Err(ServiceError::message(
                "archive accepts only key and archived",
            ));
        }
        let parsed: PatchParams = parse_params(params)?;
        let key = parsed.key;
        let order = self
            .service
            .metadata
            .snapshot_tree_leaf_to_root(&key)
            .await
            .map_err(ServiceError::message)?;
        if order.is_empty() {
            return Err(ServiceError::message(format!("session '{key}' not found")));
        }

        let mut errors = Vec::new();
        let mut root_result = None;
        for session_key in &order {
            match self.service.archive_one(session_key).await {
                Ok(value) if session_key == &key => root_result = Some(value),
                Ok(_) => {},
                Err(error) => errors.push(error.to_string()),
            }
        }
        if !errors.is_empty() {
            return Err(ServiceError::message(errors.join("; ")));
        }
        root_result.ok_or_else(|| ServiceError::message(format!("session '{key}' not found")))
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
