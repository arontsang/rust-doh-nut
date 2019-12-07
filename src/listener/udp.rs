use crate::resolver::DnsResolver;
use tokio::task;
use std::net::{SocketAddr, Ipv4Addr, Ipv6Addr};
use crate::resolver::echo::EchoResolver;

pub fn start(bind_address: SocketAddr) -> tokio::task::JoinHandle<()> {


    let join_handle: task::JoinHandle<()> = task::spawn(async move {

        let socket = tokio::net::UdpSocket::bind(bind_address).await;
        let socket = socket.unwrap();

        let (mut _receiver, mut _sender) = socket.split();

        let (reply_queue, reply_tasks) = tokio::sync::mpsc::channel::<(SocketAddr, Box<Vec<u8>>)>(100);

        let listen_request_task = task::spawn(async move {
            let mut queue = reply_tasks;
            loop {
                let (client, payload) = queue.recv().await.unwrap();
                println!("{}", &client);
                _sender.send_to(&payload, &client).await.unwrap();
            }
        });

        let resolver = async_std::sync::Arc::new(EchoResolver{});

        let request_dispatch_task = task::spawn(async move {
            let mut receiver = _receiver;
            loop {
                let mut local_reply_queue = reply_queue.clone();
                let local_resolver_copy = async_std::sync::Arc::clone(&resolver);
                let mut buffer = Box::new(vec![0]);
                {
                    let (length, client) = {
                        let buffer = buffer.as_mut();
                        buffer.resize(1500, 0);

                        receiver.recv_from(buffer).await.unwrap()
                    };
                    let port = client.port();
                    task::spawn(async move {
                        let buffer = buffer;
                        let query: &[u8] = &buffer[0usize..length];
                        let reply = local_resolver_copy.resolve(query).await;
                        local_reply_queue.send((client, reply)).await.unwrap();
                    });

                }
            }
        });

        listen_request_task.await.unwrap();
        request_dispatch_task.await.unwrap();
    });

    join_handle
}

