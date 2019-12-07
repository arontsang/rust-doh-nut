pub mod echo;
use async_trait::async_trait;

#[async_trait]
pub trait DnsResolver {

    async fn resolve(&'_ self, request: &[u8]) -> Box<Vec<u8>>;
}

