
use crate::resolver::DnsResolver;

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::{Body, Method};
use std::error::Error;

pub struct HyperResolver {
    server : String
}

lazy_static! {
    static ref CLIENT: reqwest::Client = {
        reqwest::Client::builder()
            .tcp_nodelay(true)
            .pool_max_idle_per_host(16)
            .build()
            .expect("")
    };
}



impl HyperResolver {

    pub async fn new(server: &String) -> Result<HyperResolver, Box<dyn Error>> {
        Result::Ok(HyperResolver { server: server.clone() })
    }
}

#[async_trait]
impl DnsResolver for HyperResolver {

    async fn resolve(&self, query: Bytes) -> Result<Bytes, Box<dyn Error>> {
        let length = query.len().to_string();

        let body = Body::from(query);
        let request = CLIENT.request(Method::POST, &self.server)
            .header("accept", "application/dns-message")
            .header("content-type", "application/dns-message")
            .header("content-length", length)
            .body(body);

        let response = request.send().await?;
        let body = response.bytes().await?;

        Result::Ok(body)
    }
}
