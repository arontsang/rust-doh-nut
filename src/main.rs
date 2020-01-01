use std::net::*;
use crate::resolver::hyper::HyperResolver;
use std::error::Error;
use tokio::runtime::Runtime;
use tokio::task;


mod resolver;
mod listener;

fn main() -> Result<(), Box<dyn Error>> {
    let mut rt = Runtime::new()?;
    let local = task::LocalSet::new();

    local.block_on(&mut rt, async {
        let binding_socket = SocketAddr::from(([0, 0, 0, 0], 15353));
        let resolver = HyperResolver::new().await;
        let resolver = resolver.map(|x| async_std::sync::Arc::new(x));
        let success = resolver.map(|x| crate::listener::udp::start(binding_socket, x));
        let success = match success {
            Ok(future) => future.await,
            Err(err) => Err(err),
        };
        match success {
            Ok(()) => {},
            Err(e) =>{
                eprintln!("Application error: {}", e);
            }
        }
    });
    Result::Ok(())
}