use {
    super::{tests::fixture, *},
    chelix_config::schema::ExaConfig,
    secrecy::Secret,
    std::sync::atomic::{AtomicUsize, Ordering},
    tokio::sync::Notify,
};

async fn update(config: &ConfigService, key: &'static str, timeout: u64) -> anyhow::Result<()> {
    config
        .update(move |config| {
            config.tools.exa.token = Some(Secret::new(key.into()));

            config.tools.exa.request_timeout_secs = timeout;
            Ok(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn next_http_uses_current_credentials_and_timeout_while_built_request_keeps_its_values()
-> anyhow::Result<()> {
    let fixture = fixture(ExaConfig {
        token: Some(Secret::new(" old-key ".into())),
        request_timeout_secs: 10,
    })
    .await;
    let mut server = mockito::Server::new_async().await;
    let mut mocks = Vec::new();
    for key in ["old-key", "new-key"] {
        mocks.push(
            server
                .mock("POST", "/search")
                .match_header("x-api-key", key)
                .with_status(200)
                .with_body("{}")
                .expect(1)
                .create_async()
                .await,
        );
    }
    let tool = ExaSearchTool::for_test(fixture.service.clone(), format!("{}/search", server.url()));
    let input = json!({"query": "sample"});
    let (request, timeout) = tool.make_request(&input).await?;
    assert_eq!(timeout, Duration::from_secs(10));
    let built = request
        .try_clone()
        .ok_or_else(|| anyhow::anyhow!("request should be cloneable"))?
        .build()?;
    assert_eq!(built.timeout().copied(), Some(timeout));
    update(&fixture.service, "new-key", 20).await?;
    let (response, sent_timeout, cooldown) = tool.send_once(request, timeout).await?;
    assert!(response.status().is_success());
    assert_eq!(sent_timeout, Duration::from_secs(10));
    assert!(!cooldown);
    let (request, timeout) = tool.make_request(&input).await?;
    assert_eq!(timeout, Duration::from_secs(20));
    let built = request
        .try_clone()
        .ok_or_else(|| anyhow::anyhow!("request should be cloneable"))?
        .build()?;
    assert_eq!(built.timeout().copied(), Some(timeout));
    let (response, sent_timeout, _) = tool.send_once(request, timeout).await?;
    assert!(response.status().is_success());
    assert_eq!(sent_timeout, Duration::from_secs(20));
    for mock in mocks {
        mock.assert_async().await;
    }
    Ok(())
}

#[tokio::test]
async fn retry_reads_new_credentials_before_waiting_at_the_gate() -> anyhow::Result<()> {
    let fixture = fixture(ExaConfig {
        token: Some(Secret::new(" old-key ".into())),
        request_timeout_secs: 10,
    })
    .await;
    let mut server = mockito::Server::new_async().await;
    let mut mocks = Vec::new();
    for (key, status) in [("old-key", 429), ("retry-key", 200)] {
        mocks.push(
            server
                .mock("POST", "/search")
                .match_header("x-api-key", key)
                .with_status(status)
                .with_header("retry-after", "0")
                .with_body("{}")
                .expect(1)
                .create_async()
                .await,
        );
    }
    let tool = ExaSearchTool::for_test(fixture.service.clone(), format!("{}/search", server.url()));
    let input = json!({"query": "sample"});
    let attempts = AtomicUsize::new(0);
    let retry_built = Notify::new();
    let operation = tool.send_with_retry(None, || async {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 1 {
            update(&fixture.service, "retry-key", 20).await?;
        }
        let built = tool.make_request(&input).await?;
        if attempt == 1 {
            retry_built.notify_one();
        }
        Ok(built)
    });
    let update_while_waiting = async {
        tokio::time::timeout(Duration::from_secs(10), retry_built.notified()).await?;
        update(&fixture.service, "later-key", 30).await
    };
    let (result, updated) = tokio::join!(operation, update_while_waiting);
    updated?;
    let (response, timeout) = result?;
    assert!(response.status().is_success());
    assert_eq!(timeout, Duration::from_secs(20));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    for mock in mocks {
        mock.assert_async().await;
    }
    let (request, timeout) = tool.make_request(&input).await?;
    let request = request.build()?;
    assert_eq!(timeout, Duration::from_secs(30));
    assert_eq!(request.timeout().copied(), Some(timeout));
    Ok(())
}

#[tokio::test]
async fn timeout_errors_keep_the_duration_selected_before_config_update() -> anyhow::Result<()> {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let fixture = fixture(ExaConfig {
        token: Some(Secret::new(" old-key ".into())),
        request_timeout_secs: 10,
    })
    .await;
    for send_headers in [false, true] {
        update(&fixture.service, "old-key", 1).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (accepted, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut buffer = [0; 1];
            stream.read_exact(&mut buffer).await?;
            if send_headers {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                    .await?;
            }
            let _ = accepted.send(());
            std::future::pending::<()>().await;
            drop(stream);
            Ok::<_, anyhow::Error>(())
        });
        let tool =
            ExaSearchTool::for_test(fixture.service.clone(), format!("http://{address}/search"));
        let operation = tool.run(json!({"query": "sample"}), None);
        let update_after_send = async {
            tokio::time::timeout(Duration::from_secs(5), received).await??;
            update(&fixture.service, "new-key", 30).await
        };
        let (result, updated) = tokio::join!(operation, update_after_send);
        server.abort();
        updated?;
        let error = result
            .err()
            .ok_or_else(|| anyhow::anyhow!("request should time out"))?;
        assert!(
            error
                .to_string()
                .contains("Exa request timed out after 1s:"),
            "{error}"
        );
        assert!(!error.to_string().contains("new-key"));
    }
    Ok(())
}
