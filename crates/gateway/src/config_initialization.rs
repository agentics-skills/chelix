use std::net::TcpListener;

use {chelix_config::ChelixConfig, chelix_config_service::ConfigOrigin};

pub fn initialize_config(config: &mut ChelixConfig, _: ConfigOrigin) -> chelix_config::Result<()> {
    if config.server.port == 0 {
        let address = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_err(|error| {
                chelix_config::Error::external("allocate initial server port", error)
            })?;
        config.server.port = address.port();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_assigns_zero_port_and_preserves_explicit_port() -> chelix_config::Result<()> {
        for origin in [ConfigOrigin::Created, ConfigOrigin::Existing] {
            let mut config = ChelixConfig::default();
            initialize_config(&mut config, origin)?;
            assert_ne!(config.server.port, 0);

            config.server.port = 12000;
            initialize_config(&mut config, origin)?;
            assert_eq!(config.server.port, 12000);
        }
        Ok(())
    }
}
