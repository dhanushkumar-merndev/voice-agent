//! WebSocket client helper shared by the Sarvam providers.

use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue},
    MaybeTlsStream, WebSocketStream,
};

use crate::ProviderError;

/// A connected client socket.
pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connects to `url` with one extra header (the API key), within `timeout`.
pub async fn connect(
    url: &str,
    header: &'static str,
    value: &str,
    timeout: std::time::Duration,
) -> Result<Socket, ProviderError> {
    let mut request = url
        .into_client_request()
        .map_err(|e| ProviderError::Connect(format!("bad url: {e}")))?;
    let value =
        HeaderValue::from_str(value).map_err(|_| ProviderError::Connect("bad api key".into()))?;
    request.headers_mut().insert(header, value);

    match tokio::time::timeout(timeout, connect_async(request)).await {
        Err(_) => Err(ProviderError::Timeout),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) => {
            let status = resp.status().as_u16();
            let body = resp
                .body()
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).chars().take(300).collect())
                .unwrap_or_default();
            Err(ProviderError::Status { status, body })
        }
        Ok(Err(e)) => Err(ProviderError::Connect(e.to_string())),
        Ok(Ok((socket, _))) => Ok(socket),
    }
}
