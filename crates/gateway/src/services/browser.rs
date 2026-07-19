// ── Browser (Real implementation — depends on chelix-browser) ───────────────

use super::*;

/// Real browser service using BrowserManager.
pub struct RealBrowserService {
    config: chelix_browser::BrowserConfig,
    sandbox_mode: chelix_config::schema::SandboxMode,
    manager: tokio::sync::OnceCell<Arc<chelix_browser::BrowserManager>>,
}

impl RealBrowserService {
    pub fn new(
        config: &chelix_config::schema::BrowserConfig,
        sandbox_mode: chelix_config::schema::SandboxMode,
        container_prefix: String,
        host_data_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let mut browser_config = chelix_browser::BrowserConfig::from(config);
        browser_config.container_prefix = container_prefix;
        browser_config.host_data_dir = host_data_dir;
        Self {
            config: browser_config,
            sandbox_mode,
            manager: tokio::sync::OnceCell::new(),
        }
    }

    pub fn from_config(
        config: &chelix_config::schema::ChelixConfig,
        container_prefix: String,
    ) -> Option<Self> {
        if !config.tools.browser.enabled {
            return None;
        }
        Some(Self::new(
            &config.tools.browser,
            config.sandbox.mode,
            container_prefix,
            config
                .sandbox
                .host_data_dir
                .as_ref()
                .map(std::path::PathBuf::from),
        ))
    }

    async fn manager(&self) -> Arc<chelix_browser::BrowserManager> {
        Arc::clone(
            self.manager
                .get_or_init(|| async {
                    let config = self.config.clone();
                    let sandbox_mode = self.sandbox_mode;
                    match tokio::task::spawn_blocking(move || {
                        // Browser detection and stale-container cleanup can block;
                        // run these off the async runtime worker threads.
                        chelix_browser::detect::check_and_warn(config.chrome_path.as_deref());
                        Arc::new(chelix_browser::BrowserManager::new(config, sandbox_mode))
                    })
                    .await
                    {
                        Ok(manager) => manager,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                "browser warmup worker failed, falling back to inline initialization"
                            );
                            let config = self.config.clone();
                            chelix_browser::detect::check_and_warn(config.chrome_path.as_deref());
                            Arc::new(chelix_browser::BrowserManager::new(
                                config,
                                self.sandbox_mode,
                            ))
                        },
                    }
                })
                .await,
        )
    }

    fn manager_if_initialized(&self) -> Option<Arc<chelix_browser::BrowserManager>> {
        self.manager.get().map(Arc::clone)
    }
}

#[async_trait]
impl BrowserService for RealBrowserService {
    async fn request(&self, params: Value) -> ServiceResult {
        let request: chelix_browser::BrowserRequest =
            serde_json::from_value(params).map_err(|e| format!("invalid request: {e}"))?;

        let manager = self.manager().await;
        let response = manager.handle_request(request).await;

        Ok(serde_json::to_value(&response).map_err(|e| format!("serialization error: {e}"))?)
    }

    async fn warmup(&self) {
        let started = std::time::Instant::now();
        let _ = self.manager().await;
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "browser service warmup complete"
        );
    }

    async fn cleanup_idle(&self) {
        if let Some(manager) = self.manager_if_initialized() {
            manager.cleanup_idle().await;
        }
    }

    async fn shutdown(&self) {
        if let Some(manager) = self.manager_if_initialized() {
            manager.shutdown().await;
        }
    }

    async fn close_all(&self) {
        if let Some(manager) = self.manager_if_initialized() {
            manager.shutdown().await;
        }
    }
}
