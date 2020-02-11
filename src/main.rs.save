use std::net::*;
use std::error::Error;
use crate::resolver::hyper::HyperResolver;
use tokio::task;
mod resolver;
mod listener;

fn main() -> Result<(), Box<dyn Error>> {
    let mut rt = tokio::runtime::Builder::new()
        .basic_scheduler()
        .enable_time()
        .enable_io()
        .build()?;
    let local = task::LocalSet::new();
    local.block_on(&mut rt, async move {
        let binding_socket = SocketAddr::from(([0, 0, 0, 0], 15353));
        let resolver = HyperResolver::new().await;
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
