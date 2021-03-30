use crate::resolver::DnsResolver;
use arr_macro::arr;
use bytes::{BytesMut, Bytes, BufMut};
use tokio::task;
use tokio::net::UdpSocket;
use std::net::{SocketAddr};
use std::error::Error;
use std::sync::Arc;
pub async fn start<T : DnsResolver + 'static >(bind_address: SocketAddr, resolver: std::rc::Rc<T>) -> Result<(), Box<dyn Error>> {
    let socket = Arc::new(UdpSocket::bind(bind_address).await?);
    let (receiver, sender) = (socket.clone(), socket);
    let (reply_queue, reply_tasks) = tokio::sync::mpsc::channel::<(SocketAddr, Bytes)>(100);

    let listen_request_task = task::spawn_local(async move {
        let mut queue = reply_tasks;
        let sender = sender;
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

    let (received_packet_enqueue, mut dequeue_received_packet) = tokio::sync::mpsc::channel::<(SocketAddr, Bytes)>(100);

    let receiving_task = task::spawn(async move {
        loop {
            let mut read_buffer = arr![0u8; 1500];

            match receiver.recv_from(&mut read_buffer).await {
                Ok((length, client)) => {
                    let mut buffer = BytesMut::with_capacity(1500);
                    buffer.put(&read_buffer[0..length]);

                    let query = buffer.freeze();
                    received_packet_enqueue.send((client, query)).await.unwrap();
                },
                Err(error) => {
                    println!("Error receiving packet: {}", error)
                }
            }
        }
    });

    let request_dispatch_task = task::spawn_local(async move {


        while let Some((client, query)) = dequeue_received_packet.recv().await{
            let local_resolver_copy = resolver.clone();
            let local_reply_queue = reply_queue.clone();
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
        }


    });

    listen_request_task.await?;
    request_dispatch_task.await?;
    receiving_task.await?;

    Ok(())
}

