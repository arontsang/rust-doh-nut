use crate::resolver::DnsResolver;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::{Body, Client, Method};
use std::error::Error;
use std::io::{Error as IoError, ErrorKind};
use std::time::Duration;

use super::dns;

const MAX_DNS_MESSAGE_SIZE: usize = u16::MAX as usize;

pub struct HyperResolver {
    client: Client,
    server: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn query() -> Bytes {
        Bytes::from_static(
            b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x03www\x07example\x03com\x00\x00\x01\x00\x01",
        )
    }

    fn response() -> Vec<u8> {
        let mut response = query().to_vec();
        response[2] = 0x81;
        response[3] = 0x80;
        response
    }

    async fn resolve_from_mock(status: &str, content_type: &str, body: Vec<u8>) -> Result<Bytes, Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let status = status.to_owned();
        let content_type = content_type.to_owned();
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                status,
                content_type,
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        });
        let resolver = HyperResolver::new(format!("http://{}/dns-query", address)).await?;
        let result = resolver.resolve(query()).await;
        upstream.await?;
        result
    }

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
            resolver.resolve(query()),
        )
        .await;
        upstream.abort();
        let error = result.expect("request deadline did not fire").unwrap_err();
        assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
    }

    #[tokio::test]
    async fn accepts_valid_doh_response() {
        assert_eq!(resolve_from_mock("200 OK", "application/dns-message", response()).await.unwrap(), Bytes::from(response()));
    }

    #[tokio::test]
    async fn rejects_http_error_status() {
        let error = resolve_from_mock("502 Bad Gateway", "application/dns-message", response())
            .await.unwrap_err();
        assert!(error.to_string().contains("502"));
    }

    #[tokio::test]
    async fn rejects_wrong_content_type() {
        let error = resolve_from_mock("200 OK", "text/plain", response()).await.unwrap_err();
        assert_eq!(error.downcast_ref::<IoError>().unwrap().kind(), ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn rejects_oversized_response_body() {
        let error = resolve_from_mock(
            "200 OK",
            "application/dns-message",
            vec![0; MAX_DNS_MESSAGE_SIZE + 1],
        ).await.unwrap_err();
        assert_eq!(error.downcast_ref::<IoError>().unwrap().kind(), ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn rejects_malformed_and_unmatched_dns_responses() {
        let error = resolve_from_mock("200 OK", "application/dns-message", b"not DNS".to_vec())
            .await.unwrap_err();
        assert_eq!(error.downcast_ref::<IoError>().unwrap().kind(), ErrorKind::InvalidData);

        let mut mismatched = response();
        mismatched[0] ^= 1;
        let error = resolve_from_mock("200 OK", "application/dns-message", mismatched)
            .await.unwrap_err();
        assert_eq!(error.downcast_ref::<IoError>().unwrap().kind(), ErrorKind::InvalidData);
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
        if query.len() > MAX_DNS_MESSAGE_SIZE {
            return Err(IoError::new(ErrorKind::InvalidInput, "DNS query exceeds message size limit").into());
        }
        dns::validate_query(&query)?;
        let client = &self.client;

        let request = client
            .request(Method::POST, &self.server)
            .header("accept", "application/dns-message")
            .header("content-type", "application/dns-message")
            .header("content-length", query.len().to_string())
            .body(Body::from(query.clone()));

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(IoError::new(
                ErrorKind::Other,
                format!("DoH server returned HTTP {}", response.status()),
            ).into());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        if content_type != Some("application/dns-message") {
            return Err(IoError::new(ErrorKind::InvalidData, "DoH response has invalid content type").into());
        }
        if response.content_length().map_or(false, |length| length > MAX_DNS_MESSAGE_SIZE as u64) {
            return Err(IoError::new(ErrorKind::InvalidData, "DoH response exceeds DNS message size limit").into());
        }

        let mut stream = response.bytes_stream();
        let mut body = BytesMut::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len() + chunk.len() > MAX_DNS_MESSAGE_SIZE {
                return Err(IoError::new(ErrorKind::InvalidData, "DoH response exceeds DNS message size limit").into());
            }
            body.extend_from_slice(&chunk);
        }
        dns::validate_response(&query, &body)?;
        Ok(body.freeze())
    }
}
