use {
    axum::extract::ws::{Message as BrowserMessage, WebSocket},
    futures::{SinkExt, StreamExt},
    tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message as ServiceMessage},
};

pub(crate) async fn proxy(
    browser: WebSocket,
    service: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) {
    let (mut browser_tx, mut browser_rx) = browser.split();
    let (mut service_tx, mut service_rx) = service.split();

    loop {
        tokio::select! {
            browser_message = browser_rx.next() => {
                let Some(browser_message) = browser_message else {
                    break;
                };
                let browser_message = match browser_message {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::debug!(%error, "browser terminal websocket read failed");
                        break;
                    },
                };
                let service_message = match browser_message {
                    BrowserMessage::Text(text) => ServiceMessage::Text(text.to_string().into()),
                    BrowserMessage::Binary(data) => ServiceMessage::Binary(data),
                    BrowserMessage::Ping(data) => ServiceMessage::Ping(data),
                    BrowserMessage::Pong(data) => ServiceMessage::Pong(data),
                    BrowserMessage::Close(_) => {
                        let _ = service_tx.send(ServiceMessage::Close(None)).await;
                        break;
                    },
                };
                if let Err(error) = service_tx.send(service_message).await {
                    tracing::debug!(%error, "tools service terminal websocket write failed");
                    break;
                }
            },
            service_message = service_rx.next() => {
                let Some(service_message) = service_message else {
                    break;
                };
                let service_message = match service_message {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::debug!(%error, "tools service terminal websocket read failed");
                        break;
                    },
                };
                let browser_message = match service_message {
                    ServiceMessage::Text(text) => BrowserMessage::Text(text.to_string().into()),
                    ServiceMessage::Binary(data) => BrowserMessage::Binary(data),
                    ServiceMessage::Ping(data) => BrowserMessage::Ping(data),
                    ServiceMessage::Pong(data) => BrowserMessage::Pong(data),
                    ServiceMessage::Close(_) => {
                        let _ = browser_tx.send(BrowserMessage::Close(None)).await;
                        break;
                    },
                    ServiceMessage::Frame(_) => continue,
                };
                if let Err(error) = browser_tx.send(browser_message).await {
                    tracing::debug!(%error, "browser terminal websocket write failed");
                    break;
                }
            },
        }
    }
}
