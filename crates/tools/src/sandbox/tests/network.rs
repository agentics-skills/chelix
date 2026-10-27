#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

#[test]
fn test_from_config_empty_network_defaults_to_bridge() {
    let cfg = chelix_config::schema::SandboxConfig {
        network: String::new(),
        ..Default::default()
    };
    let sc = SandboxConfig::from(&cfg);
    assert_eq!(sc.network, "bridge");
}

#[test]
fn test_from_config_trims_custom_network() {
    let cfg = chelix_config::schema::SandboxConfig {
        network: "  chelix-net  ".into(),
        ..Default::default()
    };
    let sc = SandboxConfig::from(&cfg);
    assert_eq!(sc.network, "chelix-net");
}

#[tokio::test]
async fn test_docker_network_run_args_default_bridge() {
    let fixture =
        chelix_agents::test_support::ConfigFixture::new(chelix_config::ChelixConfig::default())
            .await
            .unwrap_or_else(|error| panic!("config fixture failed: {error}"));
    let docker = DockerSandbox::new(SandboxConfig::default(), fixture.service.clone());
    assert_eq!(docker.network_run_args(), vec!["--network=bridge"]);
}

#[tokio::test]
async fn test_docker_network_run_args_custom_network() {
    let fixture =
        chelix_agents::test_support::ConfigFixture::new(chelix_config::ChelixConfig::default())
            .await
            .unwrap_or_else(|error| panic!("config fixture failed: {error}"));
    let config = SandboxConfig {
        network: "chelix-net".into(),
        ..Default::default()
    };
    let docker = DockerSandbox::new(config, fixture.service.clone());
    assert_eq!(docker.network_run_args(), vec!["--network=chelix-net"]);
}
