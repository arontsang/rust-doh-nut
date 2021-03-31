use crate::resolver::DnsResolver;
use async_executor::LocalExecutor;
use async_task::Task;
use arr_macro::arr;
use bytes::{BytesMut, Bytes, BufMut};
use tokio::net::UdpSocket;
use std::net::{SocketAddr};
use std::error::Error;
use std::sync::Arc;

pub struct UdpServer<T : DnsResolver + 'static > {

    resolver: std::marker::PhantomData<T>,
    request_dispatch_task: Task<()>,
    listen_request_task: Task<()>,
    receiving_task: Task<()>,
}

impl<T : DnsResolver + 'static> Drop for UdpServer<T> {
    fn drop(&mut self) {
        drop(&mut self.receiving_task);
        drop(&mut self.listen_request_task);
        drop(&mut self.request_dispatch_task);
    }
}
impl<T : DnsResolver + 'static> UdpServer<T> {
    pub async fn new(executor: &LocalExecutor<'_>, bind_address: SocketAddr, resolver: std::rc::Rc<T>) -> Result<UdpServer<T>, Box<dyn Error>> {


        let socket = UdpSocket::bind(bind_address).await?;
        let socket = Arc::new(socket);

        let (receiver, sender) = (socket.clone(), socket);
        let (reply_queue, reply_tasks) = tokio::sync::mpsc::channel::<(SocketAddr, Bytes)>(100);

        let listen_request_task = executor.spawn(async move {
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

        let receiving_task = executor.spawn(async move {
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

        let request_dispatch_task = executor.spawn(async move {
            let continuation_executor = LocalExecutor::new();
            let receiver_executor = LocalExecutor::new();
            let _receiver_task = receiver_executor.spawn(async {
                while let Some((client, query)) = dequeue_received_packet.recv().await{
                    let local_resolver_copy = resolver.clone();
                    let local_reply_queue = reply_queue.clone();
                    continuation_executor.spawn(async move {
                        match local_resolver_copy.resolve(query).await{
                            Err(error) => println!("Error resolving packet: {}", error),
                            Ok(reply) =>
                                match local_reply_queue.send((client, reply)).await{
                                    Err(error) => println!("Error replying packet: {}", error),
                                    Ok(_) => {},
                                }
                        };
                    })
                    .detach();
                }
            });
            
            loop {
                let continuation_poll = continuation_executor.tick();
                let receiver_task_poll = receiver_executor.tick();

                futures_lite::future::or(continuation_poll, receiver_task_poll).await;
            }
        });


        Ok(UdpServer::<T> { 
            resolver: std::marker::PhantomData,
            request_dispatch_task,
            receiving_task,
            listen_request_task
        })
    }
}

