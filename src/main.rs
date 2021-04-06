#[macro_use]
extern crate lazy_static;
use async_compat::{Compat};
use async_executor::LocalExecutor;
use std::error::Error;
use std::net::*;
use crate::resolver::hyper::HyperResolver;
use crate::listener::udp::UdpServer;
mod resolver;
mod listener;




fn main() -> smol::io::Result<()> {




    smol::block_on(Compat::new(async {
        let local_ex = LocalExecutor::new();
        let _google_server = build_dns_server(&local_ex, &"https://dns.google/dns-query".to_string(), 15353).await;  
        let _cloudflare_server = build_dns_server(&local_ex, &"https://1.1.1.1/dns-query".to_string(), 15354).await;       
        local_ex.run(kill()).await
    }))
}

async fn build_dns_server(local_executor: &LocalExecutor<'_>, resolver_address: &String, binding_port: u16) -> Result<UdpServer<HyperResolver>, Box<dyn Error>> {
    let binding_socket = SocketAddr::from(([0, 0, 0, 0], binding_port));
    let resolver = HyperResolver::new(&resolver_address).await;
    let resolver = resolver.expect("Failed to get resolver.");
    let resolver = std::rc::Rc::new(resolver);

    crate::listener::udp::UdpServer::new(&local_executor, binding_socket, resolver).await
}

async fn kill() -> smol::io::Result<()>  {
    let (s, w) = smol::channel::bounded(1);

    ctrlc::set_handler(move || {
        s.try_send(()).expect("broken");
    }).expect("broken");

    match w.recv().await {        
        _ =>     Ok(())
    } 
}