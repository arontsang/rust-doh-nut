
use crate::resolver::DnsResolver;

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::{Body, Client, Method};
use std::error::Error;


pub struct HyperResolver {
    client : Client,
    server : String
}


impl HyperResolver {

    pub async fn new(server: String) -> Result<HyperResolver, Box<dyn Error>> {

        let client = reqwest::Client::builder()
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
        let length = query.len().to_string();

        let body = Body::from(query);
        let request = client.request(Method::POST, &self.server)
            .header("accept", "application/dns-message")
            .header("content-type", "application/dns-message")
            .header("content-length", length)
            .body(body);

        let response = request.send().await?;
        let body = response.bytes().await?;

        Result::Ok(body)
    }
}
