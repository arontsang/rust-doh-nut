
use crate::resolver::DnsResolver;

use async_trait::async_trait;
use bytes::Bytes;
use h2::client;
use h2::client::SendRequest;
use http::{Request, Method};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::net::TcpStream;
use std::error::Error;

pub struct HyperResolver {
    h2 : SendRequest<Bytes>
}


impl HyperResolver {

    pub async fn new() -> Result<HyperResolver, Box<dyn Error>> {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443);
        let tcp = TcpStream::connect(socket).await?;
        let (mut h2, connection) = client::handshake(tcp)
            .await?;

        tokio::spawn(async move {
            connection.await.unwrap();
        });



        Result::Ok(HyperResolver {
            h2
        })
    }

}

#[async_trait]
impl DnsResolver for HyperResolver {

    async fn resolve(&mut self, request: &[u8]) -> Box<Vec<u8>>{
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://1.1.1.1/dns-query")
            .body(request)
            .unwrap();

        let (response, _) = self.h2.send_request(request, true)
            .unwrap();

        let (head, mut body) = response.await?.into_parts();


        Box::from(request.to_vec())
    }
}