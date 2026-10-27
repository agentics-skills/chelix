//! Shared configuration fixture construction for client and tool tests.

use {chelix_agents::test_support::ConfigFixture, chelix_config::ChelixConfig, secrecy::Secret};

pub(crate) async fn fixture(token: Option<Secret<String>>) -> ConfigFixture {
    let mut config = ChelixConfig::default();
    config.tools.linkup.token = token;
    ConfigFixture::new(config)
        .await
        .unwrap_or_else(|error| panic!("config fixture failed: {error}"))
}

pub(crate) async fn read_request(socket: &mut tokio::net::TcpStream) -> anyhow::Result<String> {
    use tokio::io::AsyncReadExt;

    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(socket.read_u8().await?);
    }
    let headers = String::from_utf8(bytes)?;
    let length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then_some(value.trim())
    });
    if let Some(length) = length {
        let mut body = vec![0; length.parse::<usize>()?];
        socket.read_exact(&mut body).await?;
    }
    Ok(headers)
}
