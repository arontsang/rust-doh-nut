use async_compat::{Compat};
use std::net::*;
use crate::resolver::hyper::HyperResolver;
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
        let binding_socket = SocketAddr::from(([0, 0, 0, 0], port));
        let resolver = HyperResolver::new(server).await;
        let resolver = resolver.map(|x| std::rc::Rc::new(x));
        let _success = resolver.map(|x| crate::listener::udp::start(binding_socket, x));
       
        kill().await?;

        Ok(())
    }))
}

async fn kill() -> smol::io::Result<()>  {
    let (s, w) = smol::channel::bounded(1);

    ctrlc::set_handler(move || {
        s.try_send(()).expect("broken");
    }).expect("broken");

    match w.recv().await {
        Ok(()) => Ok(()),
        _ =>     Ok(())
    } 
}