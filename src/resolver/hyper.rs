
use crate::resolver::DnsResolver;

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::{Body, Client, Method};
use std::error::Error;


pub struct HyperResolver {
    client : Client
}


impl HyperResolver {

    pub async fn new() -> Result<HyperResolver, Box<dyn Error>> {

        let client = reqwest::Client::builder()
            .tcp_nodelay()
            .max_idle_per_host(16)
            .build()?;

        Result::Ok(HyperResolver { client })
    }

}

#[async_trait]
impl DnsResolver for HyperResolver {

    async fn resolve(&self, query: Bytes) -> Result<Bytes, Box<dyn Error>> {

        let client = &self.client;

        let request = client.request(Method::POST, "https://cloudflare-dns.com/dns-query")
            .header("accept", "application/dns-message")
            .header("content-type", "application/dns-message")
            .header("content-length", query.len().to_string())
            .body(Body::from(query));

        let response = request.send().await?;
        let body = response.bytes().await?;

        Result::Ok(body)
    }
}
