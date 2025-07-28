use std::error::Error;
use std::net::SocketAddr;
use arr_macro::arr;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener};
use tokio::task;
use crate::resolver::DnsResolver;

pub async fn start<T : DnsResolver + 'static >(bind_address: SocketAddr, resolver: std::rc::Rc<T>) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(bind_address).await?;
    loop {
        let (mut socket, _) = listener.accept().await?;
        task::spawn_local( {
            let resolver = resolver.clone();
            async move {
                let length = socket.read_u16().await.unwrap();
                let length: usize = length.into();
                
                let mut read_buffer = arr![0u8; 9000];
                let read = {
                    let read_buffer = &mut read_buffer[..length];
                    socket.read_exact(read_buffer).await.unwrap()
                };
                
                let payload = Bytes::from_owner(read_buffer).slice(..read);
                let response = resolver.resolve(payload).await.unwrap();
                
                
                socket.write_u16(response.len() as u16).await.unwrap();
                
                socket.write_all(response.as_ref()).await.unwrap();
            }
        });
    }
}