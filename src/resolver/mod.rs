pub mod echo;
pub mod hyper;
use async_trait::async_trait;
use bytes::Bytes;
use std::error::Error;

#[async_trait]
pub trait DnsResolver {

    async fn resolve(&'_ self, request: Bytes) -> Result<Bytes, Box<dyn Error>>;
}

