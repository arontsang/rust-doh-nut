
use std::net::*;
use crate::resolver::echo::EchoResolver;
use tokio::runtime::Runtime;
use tokio::task;


mod resolver;
mod listener;

fn main() {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();


    local.block_on(&mut rt, async {
        let binding_socket = SocketAddr::from(([0, 0, 0, 0], 15353));
        let resolver = async_std::sync::Arc::new(EchoResolver{});
        crate::listener::udp::start(binding_socket, resolver).await;
    });

    println!("hello, world!");
}