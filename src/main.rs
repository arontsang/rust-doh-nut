use std::net::*;
use std::error::Error;
use crate::resolver::hyper::HyperResolver;
use tokio::task;
mod resolver;
mod listener;
use rustop::opts;

fn main() -> Result<(), Box<dyn Error>> {
    let (args, _) = opts! {
        synopsis "This is a DNS stub server that proxies to a DOH server.";
        opt port:Option<u16>, desc:"Port to host DNS proxy Defaults to: 15353";
        opt server:Option<String>, desc:"DOH Server defaults to: https://1.1.1.1/dns-query" ;
    }.parse_or_exit();
    let mut rt = tokio::runtime::Builder::new()
        .basic_scheduler()
        .enable_time()
        .enable_io()
        .build()?;
    let local = task::LocalSet::new();
    let port = args.port.unwrap_or(15353);
    let server = args.server.unwrap_or("https://1.1.1.1/dns-query".to_string());
    local.block_on(&mut rt, async move {
        let binding_socket = SocketAddr::from(([0, 0, 0, 0], port));
        let resolver = HyperResolver::new(server).await;
        let resolver = resolver.map(|x| std::rc::Rc::new(x));
        let success = resolver.map(|x| crate::listener::udp::start(binding_socket, x));
        let success = match success {
            Ok(future) => future.await,
            Err(err) => Err(err),
        };
        match success {
            Ok(()) => {},
            Err(e) => {
                eprintln!("Application error: {}", e);
            }
        }
    });
    Result::Ok(())
}
