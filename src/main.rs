use async_compat::{Compat};
use async_executor::LocalExecutor;
use std::error::Error;
use std::net::*;
use crate::resolver::hyper::HyperResolver;
use crate::listener::udp::UdpServer;
mod resolver;
mod listener;
use rustop::opts;

fn main() -> smol::io::Result<()> {


    let (args, _) = opts! {
        synopsis "This is a DNS stub server that proxies to a DOH server.";
        opt port:Option<u16>, desc:"Port to host DNS proxy Defaults to: 15353";
        opt server:Option<String>, desc:"DOH Server defaults to: https://1.1.1.1/dns-query" ;
    }.parse_or_exit();

    let port = args.port.unwrap_or(15353);
    let server = args.server.unwrap_or("https://1.1.1.1/dns-query".to_string());

    smol::block_on(Compat::new(async {
        let local_ex = LocalExecutor::new();
        let _dns_server = build_dns_server(&local_ex, server, port).await;       
        local_ex.run(kill()).await
    }))
}

async fn build_dns_server(local_executor: &LocalExecutor<'_>, resolver_address: String, binding_port: u16) -> Result<UdpServer<HyperResolver>, Box<dyn Error>> {
    let binding_socket = SocketAddr::from(([0, 0, 0, 0], binding_port));
    let resolver = HyperResolver::new(resolver_address).await;
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