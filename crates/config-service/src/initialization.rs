use std::sync::Arc;

use chelix_config::ChelixConfig;

use crate::{Error, Result, storage::Storage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigOrigin {
    Created,
    Existing,
}

pub type ConfigInitializer = fn(&mut ChelixConfig, ConfigOrigin) -> chelix_config::Result<()>;

#[derive(Clone, Default)]
pub struct ConfigInitialization {
    callbacks: Arc<Vec<ConfigInitializer>>,
}

impl ConfigInitialization {
    #[must_use]
    pub fn new(callbacks: Vec<ConfigInitializer>) -> Self {
        Self {
            callbacks: Arc::new(callbacks),
        }
    }

    fn apply(&self, config: &mut ChelixConfig, origin: ConfigOrigin) -> chelix_config::Result<()> {
        for callback in self.callbacks.iter() {
            callback(config, origin)?;
        }
        Ok(())
    }

    pub(crate) async fn prepare(&self, storage: &Storage) -> Result<(String, bool)> {
        let raw = match storage.read().await {
            Ok(raw) => raw,
            Err(Error::Read { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                let mut config = ChelixConfig::default();
                self.apply(&mut config, ConfigOrigin::Created)?;
                let raw = toml::to_string_pretty(&config).map_err(|error| {
                    chelix_config::Error::external("serialize initial configuration", error)
                })?;
                return Ok((raw, true));
            },
            Err(error) => return Err(error),
        };
        let mut document = crate::document::parse(&raw)?;
        crate::document::update(&mut document, &storage.path, |config| {
            self.apply(config, ConfigOrigin::Existing)
        })?;
        let candidate = document.to_string();
        if candidate == raw {
            Ok((raw, false))
        } else {
            Ok((candidate, true))
        }
    }
}
