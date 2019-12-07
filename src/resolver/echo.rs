use crate::resolver::DnsResolver;

use async_trait::async_trait;

pub struct EchoResolver{

}

#[async_trait]
impl DnsResolver for EchoResolver {

    async fn resolve(&'_ self, request: &[u8]) -> Box<Vec<u8>>{



        Box::from(request.to_vec())
    }
}