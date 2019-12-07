
use std::net::*;


mod resolver;
mod listener;

#[tokio::main]
async fn main() {

    let binding_socket = SocketAddr::from(([0, 0, 0, 0], 15353));


    crate::listener::udp::start(binding_socket).await.unwrap();

    println!("hello, world!");
}