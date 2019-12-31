use crate::resolver::DnsResolver;

use async_trait::async_trait;
use bytes::Bytes;
use std::error::Error;

pub struct EchoResolver{

}

#[async_trait]
impl DnsResolver for EchoResolver {

    async fn resolve(&'_ self, request: Bytes) -> Result<Bytes, Box<dyn Error>> {
        Result::Ok(request)
    }
}