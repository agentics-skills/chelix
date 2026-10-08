use super::*;

pub(super) fn register(reg: &mut MethodRegistry) {
    reg.register(
        "sessions.history.subscribe",
        Box::new(|ctx| Box::pin(crate::ui_history_subscription::subscribe(ctx))),
    );
    // Sessions
    reg.register(
        "sessions.list",
        Box::new(|ctx| {
            Box::pin(async move {
                let mut result = ctx
                    .state
                    .services
                    .session
                    .list()
                    .await
                    .map_err(ErrorShape::from)?;

                // Inject replying state so the frontend can restore the
                // thinking indicator after a full page reload.
                let active_keys = ctx.state.chat().active_session_keys().await;
                if let Some(arr) = result.as_array_mut() {
                    for entry in arr {
                        let key_str = entry.get("key").and_then(|v| v.as_str()).map(String::from);
                        if let (Some(key), Some(obj)) = (key_str, entry.as_object_mut()) {
                            obj.insert(
                                "replying".to_string(),
                                serde_json::Value::Bool(active_keys.iter().any(|k| k == &key)),
                            );
                        }
                    }
                }
                Ok(result)
            })
        }),
    );
    reg.register(
        "sessions.preview",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .preview(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.search",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .search(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.resolve",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .resolve(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.patch",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .patch(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.voice.generate",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .voice_generate(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.reset",
        Box::new(|ctx| {
            Box::pin(async move {
                let key = ctx
                    .params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let progress = crate::operation_progress::OperationProgressEmitter::new(
                    ctx.state.clone(),
                    ctx.client_conn_id.clone(),
                    ctx.request_id.clone(),
                    "sessions.reset",
                    "session_reset",
                    (!key.is_empty()).then(|| key.clone()),
                );
                let total_steps = if key.is_empty() {
                    2
                } else {
                    3
                };
                progress
                    .emit(
                        "started",
                        "Preparing to reset the session…",
                        Some(0),
                        Some(total_steps),
                        false,
                    )
                    .await;

                if !key.is_empty() {
                    let hooks = ctx.state.inner.read().await.hook_registry.clone();
                    if let Some(ref hooks) = hooks {
                        progress
                            .run_with_heartbeat(
                                "hooks",
                                "Running session reset hooks…",
                                Some(1),
                                Some(total_steps),
                                crate::session::dispatch_command_hook(hooks, &key, "reset", None),
                            )
                            .await;
                    }
                }

                progress
                    .emit(
                        "resetting",
                        "Clearing session history…",
                        Some(total_steps - 1),
                        Some(total_steps),
                        false,
                    )
                    .await;
                let result = ctx
                    .state
                    .services
                    .session
                    .reset(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from);

                match &result {
                    Ok(_) => {
                        progress
                            .emit(
                                "completed",
                                "Session reset complete",
                                Some(total_steps),
                                Some(total_steps),
                                true,
                            )
                            .await;
                    },
                    Err(_) => {
                        progress
                            .emit("failed", "Session reset failed", None, None, true)
                            .await;
                    },
                }
                result
            })
        }),
    );
    reg.register(
        "sessions.delete",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .delete(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.truncate_tail",
        Box::new(|ctx| {
            Box::pin(async move {
                let key = ctx
                    .params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ErrorShape::from(ServiceError::message("missing 'key' parameter"))
                    })?
                    .to_string();
                let mutation_reservation = ctx
                    .state
                    .services
                    .session_mutations
                    .reserve_mutation(&key)
                    .await;
                ctx.state
                    .chat()
                    .abort(serde_json::json!({ "sessionKey": key }))
                    .await
                    .map_err(ErrorShape::from)?;
                let _mutation_permit = mutation_reservation
                    .acquire()
                    .await
                    .map_err(|e| ErrorShape::from(ServiceError::message(e.to_string())))?;
                let result = ctx
                    .state
                    .services
                    .session
                    .truncate_tail(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)?;
                broadcast(
                    &ctx.state,
                    "session",
                    serde_json::json!({
                        "kind": "history_truncated",
                        "sessionKey": key,
                        "entry": result.get("entry").cloned(),
                        "generation": result.get("generation").cloned(),
                        "totalMessages": result.get("totalMessages").cloned(),
                        "prunedMediaCount": result.get("prunedMediaCount").cloned(),
                    }),
                    BroadcastOpts::default(),
                )
                .await;
                Ok(result)
            })
        }),
    );
    reg.register(
        "sessions.clear_all",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .clear_all()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.compact",
        Box::new(|ctx| {
            Box::pin(async move {
                let key = ctx
                    .params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);
                let progress = crate::operation_progress::OperationProgressEmitter::new(
                    ctx.state.clone(),
                    ctx.client_conn_id.clone(),
                    ctx.request_id.clone(),
                    "sessions.compact",
                    "session_compact",
                    key,
                );
                progress
                    .emit(
                        "started",
                        "Preparing to compact the context window…",
                        Some(0),
                        Some(2),
                        false,
                    )
                    .await;
                let result = progress
                    .run_with_heartbeat(
                        "compacting",
                        "Compacting the context window…",
                        Some(1),
                        Some(2),
                        ctx.state.services.session.compact(ctx.params.clone()),
                    )
                    .await
                    .map_err(ErrorShape::from);
                match &result {
                    Ok(_) => {
                        progress
                            .emit(
                                "completed",
                                "Context window compaction complete",
                                Some(2),
                                Some(2),
                                true,
                            )
                            .await;
                    },
                    Err(_) => {
                        progress
                            .emit(
                                "failed",
                                "Context window compaction failed",
                                None,
                                None,
                                true,
                            )
                            .await;
                    },
                }
                result
            })
        }),
    );

    reg.register(
        "sessions.fork",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .fork(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.branches",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .branches(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.run_detail",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .run_detail(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.generate_title",
        Box::new(|ctx| {
            Box::pin(async move {
                let key = ctx
                    .params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ErrorShape::new(error_codes::INVALID_REQUEST, "missing 'key' parameter")
                    })?
                    .to_string();
                let generated = crate::session::title::generate_title_for_session(&ctx.state, &key)
                    .await
                    .map_err(|e| ErrorShape::new(error_codes::UNAVAILABLE, e.to_string()))?;
                let label = if generated.is_some() {
                    generated
                } else if let Some(ref metadata) = ctx.state.services.session_metadata {
                    metadata
                        .get(&key)
                        .await
                        .map_err(|error| {
                            ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                        })?
                        .and_then(|entry| entry.label)
                } else {
                    None
                };
                Ok(serde_json::json!({ "ok": true, "label": label }))
            })
        }),
    );
    reg.register(
        "sessions.share.create",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .share_create(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.share.list",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .share_list(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "sessions.share.revoke",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .session
                    .share_revoke(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use {
        async_trait::async_trait,
        chelix_service_traits::{
            ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
            ChatRawPromptRequest, ChatSendRequest, ChatSendSyncRequest, ChatService, ServiceResult,
        },
        serde_json::Value,
    };

    use crate::{
        auth::{AuthMode, ResolvedAuth},
        methods::{MethodContext, MethodRegistry, MethodTransport},
        services::GatewayServices,
        state::GatewayState,
    };

    struct AbortFails;

    #[async_trait]
    impl ChatService for AbortFails {
        async fn send(
            &self,
            _request: ChatSendRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("unused".into())
        }

        async fn send_sync(
            &self,
            _request: ChatSendSyncRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("unused".into())
        }

        async fn abort(&self, _params: Value) -> ServiceResult {
            Err("stop failed".into())
        }

        async fn history(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn inject(&self, _params: Value) -> ServiceResult {
            Err("unused".into())
        }

        async fn clear(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({ "ok": true }))
        }

        async fn compact(
            &self,
            _request: ChatCompactRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("unused".into())
        }

        async fn context(
            &self,
            _request: ChatContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn raw_prompt(
            &self,
            _request: ChatRawPromptRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("unused".into())
        }

        async fn full_context(
            &self,
            _request: ChatFullContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Ok(serde_json::json!({}))
        }
    }

    #[tokio::test]
    async fn truncate_tail_returns_abort_error_before_acquire()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let dir = tempfile::tempdir()?;
        let store = Arc::new(chelix_sessions::store::SessionStore::new(
            dir.path().to_path_buf(),
        ));
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        chelix_projects::run_migrations(&pool).await?;
        chelix_sessions::metadata::SqliteSessionMetadata::init(&pool).await?;
        let metadata = Arc::new(chelix_sessions::metadata::SqliteSessionMetadata::new(pool));
        let pair = chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("off"),
        )?;
        metadata
            .create_llm_session("child", None, &pair, Some("main"))
            .await?;
        store
            .append(
                "child",
                &serde_json::json!({"role": "user", "content": "keep"}),
            )
            .await?;
        let session = Arc::new(crate::session::LiveSessionService::new(
            Arc::clone(&store),
            metadata,
        ));
        let mut services = GatewayServices::noop();
        let mutations = Arc::clone(&services.session_mutations);
        services.session = session;
        let state = GatewayState::new(
            ResolvedAuth {
                mode: AuthMode::Token,
                token: None,
                password: None,
            },
            services,
        );
        state.set_chat(Arc::new(AbortFails));
        let permit = mutations
            .try_acquire_turn("child")
            .await
            .unwrap_or_else(|error| panic!("permit: {error}"));
        let registry = MethodRegistry::new();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            registry.dispatch(MethodContext {
                request_id: "1".into(),
                method: "sessions.truncate_tail".into(),
                params: serde_json::json!({"key": "child"}),
                client_conn_id: "conn".into(),
                transport: MethodTransport::StatefulConnection,
                client_role: "operator".into(),
                client_scopes: vec!["operator.write".to_string()],
                state,
                channel: None,
            }),
        )
        .await?;
        assert!(!response.ok);
        assert!(
            response
                .error
                .as_ref()
                .is_some_and(|error| error.message.contains("stop failed"))
        );
        assert_eq!(store.read("child").await?.len(), 1);
        drop(permit);
        Ok(())
    }
}
