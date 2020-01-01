use crate::resolver::DnsResolver;
use arr_macro::arr;
use bytes::{BytesMut, Bytes, BufMut};
use tokio::task;
use std::net::{SocketAddr};
use std::error::Error;

pub async fn start<T : DnsResolver + 'static >(bind_address: SocketAddr, resolver: async_std::sync::Arc<T>) -> Result<(), Box<dyn Error>> {


    let socket = tokio::net::UdpSocket::bind(bind_address).await;
    let socket = socket?;

    let (mut receiver, mut sender) = socket.split();

    let (reply_queue, reply_tasks) = tokio::sync::mpsc::channel::<(SocketAddr, Bytes)>(100);

    let listen_request_task = task::spawn_local(async move {
        let mut queue = reply_tasks;
        loop {
            let success = match queue.recv().await {
                Some((client, payload)) =>
                    Some(sender.send_to(&payload, &client).await),
                None => None,
            };
            match success {
                Some(Ok(_)) => {},
                Some(Err(error)) =>
                    println!("Error sending reply: {}", error),
                None => {},
            }
        }
    });


    let request_dispatch_task = task::spawn_local(async move {
        loop {
            let mut local_reply_queue = reply_queue.clone();
            let local_resolver_copy = resolver.clone();
            let mut buffer = BytesMut::with_capacity(1500);
            {
                let mut read_buffer = arr![0u8; 1500];
                match receiver.recv_from(&mut read_buffer).await{
                    Ok((length, client)) => {
                        buffer.put(&read_buffer[0..length]);

                        let query = buffer.freeze();

                        task::spawn_local(async move {

                            match local_resolver_copy.resolve(query).await{
                                Err(error) => println!("Error resolving packet: {}", error),
                                Ok(reply) =>
                                    match local_reply_queue.send((client, reply)).await{
                                        Err(error) => println!("Error replying packet: {}", error),
                                        Ok(_) => {},
                                    }
                            };
                        });
                    },
                    Err(error) =>
                        println!("Error receiving packet: {}", error),
                }
            }
        }
    });

    listen_request_task.await?;
    request_dispatch_task.await?;
    Ok(())
}

