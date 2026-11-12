use anyhow::{Result, bail, ensure};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use std::time::Duration;
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, sleep_until},
};

pub struct Http {
    pub client: Client,
    next: Mutex<Instant>,
    interval: Duration,
}

#[derive(Debug)]
pub struct Uncached;
impl std::fmt::Display for Uncached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider requires an API key for uncached tracks")
    }
}
impl std::error::Error for Uncached {}

impl Http {
    pub fn new(client: Client, interval: Duration) -> Self {
        Self {
            client,
            interval,
            next: Mutex::new(Instant::now()),
        }
    }
    async fn slot(&self) {
        let mut next = self.next.lock().await;
        sleep_until(*next).await;
        *next = Instant::now() + self.interval;
    }
    pub async fn bytes(&self, request: RequestBuilder, limit: usize) -> Result<Option<Vec<u8>>> {
        for attempt in 0..3 {
            self.slot().await;
            let request = request
                .try_clone()
                .ok_or_else(|| anyhow::anyhow!("HTTP body cannot be retried"))?;
            let mut request = request.build()?;
            if let Some(query) = request.url().query() {
                let query = query.replace('+', "%20");
                request.url_mut().set_query(Some(&query));
            }
            let mut response = self.client.execute(request).await?;
            let status = response.status();
            if matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::GONE | StatusCode::FORBIDDEN
            ) {
                return Ok(None);
            }
            if status == StatusCode::UNAUTHORIZED {
                return Err(Uncached.into());
            }
            if matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) && attempt < 2
            {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(2u64.pow(attempt + 1));
                ensure!(
                    delay <= 30,
                    "provider requested retry after {delay}s; try again later"
                );
                sleep(Duration::from_secs(delay)).await;
                continue;
            }
            if !status.is_success() {
                bail!("lyrics API returned HTTP {status}");
            }
            ensure!(
                response.content_length().is_none_or(|n| n <= limit as u64),
                "response exceeds size limit"
            );
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    bytes.len() + chunk.len() <= limit,
                    "response exceeds size limit"
                );
                bytes.extend_from_slice(&chunk);
            }
            return Ok(Some(bytes));
        }
        unreachable!()
    }
    pub async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<Option<T>> {
        self.bytes(request, crate::lyrics::MAX_BYTES)
            .await?
            .map(|bytes| Ok(serde_json::from_slice(&bytes)?))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    #[tokio::test]
    async fn retries_429_and_bounds_chunked_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for response in [
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
            ] {
                let (mut conn, _) = listener.accept().await.unwrap();
                let mut buf = [0; 1024];
                conn.read(&mut buf).await.unwrap();
                conn.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let http = Http::new(Client::new(), Duration::ZERO);
        assert!(
            http.bytes(http.client.get(url), 4)
                .await
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
        server.await.unwrap();
    }
}
