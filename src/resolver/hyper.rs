use crate::resolver::DnsResolver;

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::{Body, Client, Method};
use std::error::Error;
use std::time::Duration;

pub struct HyperResolver {
    client: Client,
    server: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn request_timeout_includes_response_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let resolver = HyperResolver::new(format!(
            "http://{}/dns-query",
            listener.local_addr().unwrap()
        ))
        .await
        .unwrap();
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            socket.read(&mut buffer).await.unwrap();
            socket.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/dns-message\r\nContent-Length: 12\r\n\r\n"
            ).await.unwrap();
            // Send headers but never complete the response body.
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            resolver.resolve(Bytes::from_static(b"dns")),
        )
        .await;
        upstream.abort();
        let error = result.expect("request deadline did not fire").unwrap_err();
        assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
    }
}

impl HyperResolver {
    pub async fn new(server: String) -> Result<HyperResolver, Box<dyn Error>> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .pool_max_idle_per_host(16)
            .build()?;

        Result::Ok(HyperResolver { client, server })
    }
}

#[async_trait]
impl DnsResolver for HyperResolver {
    async fn resolve(&self, query: Bytes) -> Result<Bytes, Box<dyn Error>> {
        let client = &self.client;

        let request = client
            .request(Method::POST, &self.server)
            .header("accept", "application/dns-message")
            .header("content-type", "application/dns-message")
            .header("content-length", query.len().to_string())
            .body(Body::from(query));

        let response = request.send().await?;
        let body = response.bytes().await?;

        Result::Ok(body)
    }
}
