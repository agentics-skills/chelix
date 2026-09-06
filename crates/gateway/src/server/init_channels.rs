use std::{collections::HashSet, sync::Arc};

use {anyhow::Context, tracing::info};

use chelix_channels::ChannelPlugin;

use crate::services::GatewayServices;

/// Return type for channel initialization, carrying handles that the caller
/// needs to store in gateway state or pass to other init phases.
pub(crate) struct ChannelInitResult {
    pub(crate) services: GatewayServices,
    #[cfg(feature = "telephony")]
    pub(crate) telephony_webhook_plugin:
        Arc<tokio::sync::RwLock<chelix_telephony::TelephonyPlugin>>,
}

/// Wire the channel store, channel registry, and all channel plugins.
///
/// Extracted from `prepare_gateway_core` for readability.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn init_channels(
    mut services: GatewayServices,
    config: &chelix_config::ChelixConfig,
    db_pool: sqlx::SqlitePool,
    #[cfg(feature = "vault")] vault: Option<Arc<chelix_vault::Vault>>,
    message_log: Arc<dyn chelix_channels::message_log::MessageLog>,
    session_metadata: Arc<chelix_sessions::metadata::SqliteSessionMetadata>,
    deferred_state: Arc<tokio::sync::OnceCell<Arc<crate::state::GatewayState>>>,
    data_dir: &std::path::Path,
) -> anyhow::Result<ChannelInitResult> {
    use chelix_channels::{
        registry::{ChannelRegistry, RegistryOutboundRouter},
        store::ChannelStore,
    };

    let channel_store: Arc<dyn ChannelStore> = Arc::new(
        crate::channel_store::SqliteChannelStore::new(db_pool.clone()),
    );
    let db_path = data_dir.join("chelix.db");
    let stored = channel_store
        .list()
        .await
        .with_context(|| format!("failed to load stored channels from {}", db_path.display()))?;
    validate_stored_channel_types(&stored, &db_path)?;

    #[cfg(feature = "vault")]
    let channel_store: Arc<dyn ChannelStore> = Arc::new(
        crate::channel_store::VaultChannelStore::new(channel_store, vault.clone()),
    );

    let channel_sink: Arc<dyn chelix_channels::ChannelEventSink> = Arc::new(
        crate::channel_events::GatewayChannelEventSink::new(Arc::clone(&deferred_state)),
    );

    // Create plugins and register with the registry.
    let mut registry = ChannelRegistry::new();

    #[cfg(feature = "telegram")]
    {
        let tg_plugin = Arc::new(tokio::sync::RwLock::new(
            chelix_telegram::TelegramPlugin::new()
                .with_message_log(Arc::clone(&message_log))
                .with_event_sink(Arc::clone(&channel_sink)),
        ));
        registry
            .register(tg_plugin as Arc<tokio::sync::RwLock<dyn ChannelPlugin>>)
            .await;
    }

    #[cfg(feature = "matrix")]
    {
        let matrix_plugin = Arc::new(tokio::sync::RwLock::new(
            chelix_matrix::MatrixPlugin::new()
                .with_message_log(Arc::clone(&message_log))
                .with_event_sink(Arc::clone(&channel_sink)),
        ));
        registry
            .register(matrix_plugin as Arc<tokio::sync::RwLock<dyn ChannelPlugin>>)
            .await;
    }

    #[cfg(feature = "signal")]
    {
        let signal_plugin = Arc::new(tokio::sync::RwLock::new(
            chelix_signal::SignalPlugin::new()
                .with_message_log(Arc::clone(&message_log))
                .with_event_sink(Arc::clone(&channel_sink)),
        ));
        registry
            .register(signal_plugin as Arc<tokio::sync::RwLock<dyn ChannelPlugin>>)
            .await;
    }

    #[cfg(feature = "whatsapp")]
    {
        let wa_data_dir = data_dir.join("whatsapp");
        if let Err(e) = std::fs::create_dir_all(&wa_data_dir) {
            tracing::warn!("failed to create whatsapp data dir: {e}");
        }
        let whatsapp_plugin = Arc::new(tokio::sync::RwLock::new(
            chelix_whatsapp::WhatsAppPlugin::new(wa_data_dir)
                .with_message_log(Arc::clone(&message_log))
                .with_event_sink(Arc::clone(&channel_sink)),
        ));
        registry
            .register(whatsapp_plugin as Arc<tokio::sync::RwLock<dyn ChannelPlugin>>)
            .await;
    }
    #[cfg(not(feature = "whatsapp"))]
    let _ = &channel_sink; // silence unused warning

    #[cfg(feature = "telephony")]
    let telephony_webhook_plugin: Arc<tokio::sync::RwLock<chelix_telephony::TelephonyPlugin>>;
    #[cfg(feature = "telephony")]
    {
        let telephony_plugin = Arc::new(tokio::sync::RwLock::new(
            chelix_telephony::TelephonyPlugin::new()
                .with_message_log(Arc::clone(&message_log))
                .with_event_sink(Arc::clone(&channel_sink)),
        ));
        telephony_webhook_plugin = Arc::clone(&telephony_plugin);
        registry
            .register(telephony_plugin as Arc<tokio::sync::RwLock<dyn ChannelPlugin>>)
            .await;
    }

    // Collect all channel accounts to start (config + stored), then
    // spawn them concurrently so slow network calls (e.g. Telegram)
    // don't block startup sequentially.
    let mut pending_starts: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut queued: HashSet<(String, String)> = HashSet::new();

    #[cfg(feature = "telephony")]
    if let Some((account_id, account_config)) = crate::methods::phone::phone_channel_account(config)
    {
        let key = ("telephony".to_string(), account_id.clone());
        if registry.get("telephony").is_some() && queued.insert(key) {
            pending_starts.push(("telephony".to_string(), account_id, account_config));
        }
    }

    for (channel_type, accounts) in config.channels.all_channel_configs() {
        if registry.get(channel_type).is_none() {
            if !accounts.is_empty() {
                tracing::debug!(
                    channel_type,
                    "skipping config — no plugin registered for this channel type"
                );
            }
            continue;
        }
        for (account_id, account_config) in accounts {
            let key = (channel_type.to_string(), account_id.clone());
            if queued.insert(key) {
                pending_starts.push((
                    channel_type.to_string(),
                    account_id.clone(),
                    account_config.clone(),
                ));
            }
        }
    }

    // Load persisted channels that were not queued from config.
    match channel_store.list().await {
        Ok(stored) => {
            info!("{} stored channel(s) found in database", stored.len());
            for ch in stored {
                let key = (ch.channel_type.clone(), ch.account_id.clone());
                if queued.contains(&key) {
                    info!(
                        account_id = ch.account_id,
                        channel_type = ch.channel_type,
                        "skipping stored channel (already started from config)"
                    );
                    continue;
                }
                if registry.get(&ch.channel_type).is_none() {
                    tracing::warn!(
                        account_id = ch.account_id,
                        channel_type = ch.channel_type,
                        "unsupported channel type, skipping stored account"
                    );
                    continue;
                }
                info!(
                    account_id = ch.account_id,
                    channel_type = ch.channel_type,
                    "starting stored channel"
                );
                if queued.insert(key) {
                    pending_starts.push((ch.channel_type, ch.account_id, ch.config));
                }
            }
        },
        Err(e) => tracing::warn!("failed to load stored channels: {e}"),
    }

    let registry = Arc::new(registry);

    // Spawn all channel starts concurrently.
    if !pending_starts.is_empty() {
        let total = pending_starts.len();
        info!("{total} channel account(s) queued for startup");
        for (channel_type, account_id, account_config) in pending_starts {
            let reg = Arc::clone(&registry);
            tokio::spawn(async move {
                if let Err(e) = reg
                    .start_account(&channel_type, &account_id, account_config)
                    .await
                {
                    tracing::warn!(
                        account_id,
                        channel_type,
                        "failed to start channel account: {e}"
                    );
                } else {
                    info!(account_id, channel_type, "channel account started");
                }
            });
        }
    }
    let router = Arc::new(RegistryOutboundRouter::new(Arc::clone(&registry)));

    services = services.with_channel_registry(Arc::clone(&registry));
    services = services.with_channel_store(Arc::clone(&channel_store));
    let outbound_router = Arc::clone(&router) as Arc<dyn chelix_channels::ChannelOutbound>;
    services = services.with_channel_outbound(Arc::clone(&outbound_router));
    services = services
        .with_channel_stream_outbound(router as Arc<dyn chelix_channels::ChannelStreamOutbound>);

    services.channel = Arc::new(crate::channel::LiveChannelService::new(
        registry,
        outbound_router,
        channel_store,
        Arc::clone(&message_log),
        Arc::clone(&session_metadata),
    ));

    Ok(ChannelInitResult {
        services,
        #[cfg(feature = "telephony")]
        telephony_webhook_plugin,
    })
}

fn validate_stored_channel_types(
    channels: &[chelix_channels::store::StoredChannel],
    db_path: &std::path::Path,
) -> anyhow::Result<()> {
    let mut unknown: Vec<_> = channels
        .iter()
        .filter(|channel| {
            channel
                .channel_type
                .parse::<chelix_channels::ChannelType>()
                .is_err()
        })
        .map(|channel| format!("{}:{}", channel.channel_type, channel.account_id))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort();
    anyhow::bail!(
        "unknown stored channel types in {}: {}",
        db_path.display(),
        unknown.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use {
        super::validate_stored_channel_types,
        chelix_channels::{ChannelType, store::StoredChannel},
    };

    fn stored_channel(channel_type: &str, account_id: &str) -> StoredChannel {
        StoredChannel {
            account_id: account_id.into(),
            channel_type: channel_type.into(),
            config: serde_json::json!({}),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn stored_channel_types_accept_supported_accounts() {
        let channels: Vec<_> = ChannelType::ALL
            .iter()
            .map(|channel_type| stored_channel(channel_type.as_str(), "bot"))
            .collect();
        assert!(
            validate_stored_channel_types(&channels, std::path::Path::new("chelix.db")).is_ok()
        );
    }

    #[test]
    fn stored_channel_types_report_all_unknown_accounts_and_database_path() {
        let channels = vec![
            stored_channel("telegram", "known"),
            stored_channel("unknown-b", "bot2"),
            stored_channel("unknown-a", "bot1"),
        ];
        let result =
            validate_stored_channel_types(&channels, std::path::Path::new("data/chelix.db"));
        let Err(error) = result else {
            panic!("unknown types must prevent channel startup");
        };
        let error = error.to_string();
        assert!(error.contains("data/chelix.db"), "{error}");
        assert!(error.contains("unknown-a:bot1, unknown-b:bot2"), "{error}");
        assert!(!error.contains("telegram:known"), "{error}");
    }
}
