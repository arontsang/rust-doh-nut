
use std::net::*;
use crate::resolver::hyper::HyperResolver;
use std::error::Error;
use tokio::runtime::Runtime;
use tokio::task;


mod resolver;
mod listener;

fn main() -> Result<(), Box<dyn Error>> {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();


    local.block_on(&mut rt, async {
        let binding_socket = SocketAddr::from(([0, 0, 0, 0], 15353));
        let resolver = HyperResolver::new().await;
        let resolver: HyperResolver = resolver.unwrap();
        let resolver = async_std::sync::Arc::new(resolver);
        crate::listener::udp::start(binding_socket, resolver).await;


    });

    println!("hello, world!");
    Result::Ok(())
}