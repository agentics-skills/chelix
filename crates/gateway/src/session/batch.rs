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
            return Err(ServiceError::message("missing 'key' parameter"));
        };
        let force = params
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let order = self
            .service
            .metadata
            .snapshot_tree_leaf_to_root(key)
            .await
            .map_err(ServiceError::message)?;
        if order.is_empty() {
            return Err(ServiceError::message(format!("session '{key}' not found")));
        }

        let mut errors = Vec::new();
        for session_key in &order {
            if let Err(error) = self.service.delete_one(session_key, force).await {
                errors.push(format!("session '{session_key}': {error}"));
            }
        }
        if errors.is_empty() {
            Ok(serde_json::json!({ "ok": true }))
        } else {
            Err(ServiceError::message(errors.join("; ")))
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
}
