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

#[test]
fn test_docker_network_run_args_default_bridge() {
    let docker = DockerSandbox::new(SandboxConfig::default());
    assert_eq!(docker.network_run_args(), vec!["--network=bridge"]);
}

#[test]
fn test_docker_network_run_args_custom_network() {
    let config = SandboxConfig {
        network: "chelix-net".into(),
        ..Default::default()
    };
    let docker = DockerSandbox::new(config);
    assert_eq!(docker.network_run_args(), vec!["--network=chelix-net"]);
}
