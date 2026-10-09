use crate::resolver::{dns, DnsResolver};
use arr_macro::arr;
use bytes::{BufMut, Bytes, BytesMut};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Semaphore;
use tokio::task;

pub async fn start<T: DnsResolver + 'static>(
    bind_address: SocketAddr,
    resolver: std::rc::Rc<T>,
    lookup_slots: Arc<Semaphore>,
) -> Result<(), Box<dyn Error>> {
    let socket = UdpSocket::bind(bind_address).await?;
    let socket = Arc::new(socket);
    let (receiver, sender) = (socket.clone(), socket.clone());
    let (reply_queue, reply_tasks) = tokio::sync::mpsc::channel::<(SocketAddr, Bytes)>(100);

    let listen_request_task = task::spawn_local(async move {
        let mut queue = reply_tasks;
        let sender = sender;
        loop {
            let success = match queue.recv().await {
                Some((client, payload)) => Some(sender.send_to(&payload, &client).await),
                None => None,
            };
            match success {
                Some(Ok(_)) => {}
                Some(Err(error)) => println!("Error sending reply: {}", error),
                None => {}
            }
        }
    });

    let (received_packet_enqueue, mut dequeue_received_packet) =
        tokio::sync::mpsc::channel::<(SocketAddr, Bytes)>(100);

    let receiving_task = task::spawn(async move {
        loop {
            let mut read_buffer = arr![0u8; 1500];

            match receiver.recv_from(&mut read_buffer).await {
                Ok((length, client)) => {
                    let mut buffer = BytesMut::with_capacity(1500);
                    buffer.put(&read_buffer[0..length]);

                    let query = buffer.freeze();
                    received_packet_enqueue.send((client, query)).await.unwrap();
                }
                Err(error) => {
                    println!("Error receiving packet: {}", error)
                }
            }
        }
    });

    let request_dispatch_task = task::spawn_local(async move {
        while let Some((client, query)) = dequeue_received_packet.recv().await {
            // Acquire before spawning so overload cannot create unlimited waiters.
            let slot = lookup_slots.clone().acquire_owned().await?;
            let local_resolver_copy = resolver.clone();
            let local_reply_queue = reply_queue.clone();
            task::spawn_local(async move {
                let _slot = slot;
                let reply = match local_resolver_copy.resolve(query.clone()).await {
                    Ok(reply) => Some(reply),
                    Err(error) => {
                        eprintln!("Error resolving packet: {}", error);
                        match dns::servfail_response(&query) {
                            Ok(reply) => Some(reply),
                            Err(error) => {
                                eprintln!("Cannot build DNS failure response: {}", error);
                                None
                            }
                        }
                    }
                };
                if let Some(reply) = reply {
                    if let Err(error) = local_reply_queue.send((client, reply)).await {
                        eprintln!("Error replying packet: {}", error);
                    }
                }
            });
        }
        Ok::<_, Box<dyn Error>>(())
    });

    listen_request_task.await?;
    request_dispatch_task.await??;
    receiving_task.await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::dns;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingResolver;

    #[async_trait::async_trait]
    impl DnsResolver for FailingResolver {
        async fn resolve(&self, _: Bytes) -> Result<Bytes, Box<dyn Error>> {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "upstream unavailable",
            )
            .into())
        }
    }

    #[tokio::test]
    async fn resolver_failure_returns_servfail() {
        let reservation = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let local = task::LocalSet::new();
        local
            .run_until(async {
                let server = task::spawn_local(start(
                    address,
                    std::rc::Rc::new(FailingResolver),
                    Arc::new(Semaphore::new(1)),
                ));
                tokio::task::yield_now().await;

                let query = Bytes::from_static(
                    b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x03www\x07example\x03com\x00\x00\x01\x00\x01",
                );
                let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                client.send_to(&query, address).await.unwrap();
                let mut response = [0; 512];
                let (length, _) = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    client.recv_from(&mut response),
                )
                .await
                .unwrap()
                .unwrap();
                let response = &response[..length];
                dns::validate_response(&query, response).unwrap();
                assert_eq!(u16::from_be_bytes([response[2], response[3]]) & 0xf, 2);
                server.abort();
            })
            .await;
    }

    struct GatedResolver {
        started: AtomicUsize,
        gate: Semaphore,
    }

    #[async_trait::async_trait]
    impl DnsResolver for GatedResolver {
        async fn resolve(&self, query: Bytes) -> Result<Bytes, Box<dyn Error>> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.gate.acquire().await?.forget();
            Ok(query)
        }
    }

    #[tokio::test]
    async fn udp_and_tcp_share_lookup_capacity() {
        let reservation = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let local = task::LocalSet::new();
        local
            .run_until(async {
                let slots = Arc::new(Semaphore::new(1));
                let resolver = std::rc::Rc::new(GatedResolver {
                    started: AtomicUsize::new(0),
                    gate: Semaphore::new(0),
                });
                let udp = task::spawn_local(start(address, resolver.clone(), slots.clone()));
                let tcp = task::spawn_local(crate::listener::tcp::start(
                    address,
                    resolver.clone(),
                    slots.clone(),
                ));
                tokio::task::yield_now().await;
                let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                client.send_to(b"dns", address).await.unwrap();
                timeout_started(&resolver, 1).await;
                let mut tcp_client = tokio::net::TcpStream::connect(address).await.unwrap();
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                tcp_client
                    .write_all(&[0, 3, b'd', b'n', b's'])
                    .await
                    .unwrap();
                // The TCP lookup must not enter the resolver while UDP owns the slot.
                assert!(tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    tcp_client.read_u16()
                )
                .await
                .is_err());
                assert_eq!(resolver.started.load(Ordering::SeqCst), 1);
                resolver.gate.add_permits(1);
                let mut reply = [0; 3];
                tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut reply))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(&reply, b"dns");
                timeout_started(&resolver, 2).await;
                resolver.gate.add_permits(1);
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), tcp_client.read_u16())
                        .await
                        .unwrap()
                        .unwrap(),
                    3
                );
                tcp_client.read_exact(&mut reply).await.unwrap();
                assert_eq!(&reply, b"dns");
                assert_eq!(slots.available_permits(), 1);
                udp.abort();
                tcp.abort();
            })
            .await;
    }

    async fn timeout_started(resolver: &GatedResolver, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while resolver.started.load(Ordering::SeqCst) != count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
