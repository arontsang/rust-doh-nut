use crate::resolver::{dns, DnsResolver};
use bytes::Bytes;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::convert::TryFrom;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Semaphore};
use tokio::task;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_IN_FLIGHT: usize = 16;
const MAX_CONNECTIONS: usize = 128;

pub async fn start<T: DnsResolver + 'static>(
    bind_address: SocketAddr,
    resolver: std::rc::Rc<T>,
    lookup_slots: Arc<Semaphore>,
) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(bind_address).await?;
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (mut socket, _) = listener.accept().await?;
        let connection = match connections.clone().try_acquire_owned() {
            Ok(connection) => connection,
            Err(_) => continue,
        };
        task::spawn_local({
            let resolver = resolver.clone();
            let lookup_slots = lookup_slots.clone();
            async move {
                let _connection = connection;
                if let Err(error) =
                    handle_connection(&mut socket, resolver.as_ref(), lookup_slots).await
                {
                    eprintln!("Error handling TCP query: {}", error);
                }
            }
        });
    }
}

async fn handle_connection<S, T>(
    socket: &mut S,
    resolver: &T,
    lookup_slots: Arc<Semaphore>,
) -> Result<(), Box<dyn Error>>
where
    S: AsyncRead + AsyncWrite + Unpin,
    T: DnsResolver,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let (queries, mut incoming) = mpsc::channel(MAX_IN_FLIGHT);

    let receive = async move {
        loop {
            let slot = slots.clone().acquire_owned().await?;
            let query = timeout(IO_TIMEOUT, async {
                let mut prefix = [0; 2];
                // EOF between frames is clean; EOF inside a frame is an error.
                if reader.read(&mut prefix[..1]).await? == 0 {
                    return Ok::<_, std::io::Error>(None);
                }
                reader.read_exact(&mut prefix[1..]).await?;
                let length = usize::from(u16::from_be_bytes(prefix));
                let mut buffer = vec![0; length];
                reader.read_exact(&mut buffer).await?;
                Ok(Some(Bytes::from(buffer)))
            })
            .await??;
            match query {
                Some(query) => queries.send((query, slot)).await?,
                None => return Ok::<_, Box<dyn Error>>(()),
            }
        }
    };

    let respond = async move {
        let mut pending = FuturesUnordered::new();
        let mut input_open = true;
        while input_open || !pending.is_empty() {
            tokio::select! {
                query = incoming.recv(), if input_open => {
                    match query {
                        Some((query, slot)) => {
                            let lookup_slots = lookup_slots.clone();
                            pending.push(async move {
                                let lookup_slot = timeout(IO_TIMEOUT, lookup_slots.acquire_owned()).await??;
                                let response = match resolver.resolve(query.clone()).await {
                                    Ok(response) => response,
                                    Err(error) => {
                                        eprintln!("Error resolving TCP query: {}", error);
                                        dns::servfail_response(&query)?
                                    }
                                };
                                Ok::<_, Box<dyn Error>>((response, slot, lookup_slot))
                            });
                        }
                        None => input_open = false,
                    }
                }
                Some(result) = pending.next(), if !pending.is_empty() => {
                    // Hold the slot until the complete response frame is written.
                    let (response, _slot, _lookup_slot) = result?;
                    let length = u16::try_from(response.len())?;
                    timeout(IO_TIMEOUT, async {
                        writer.write_u16(length).await?;
                        writer.write_all(response.as_ref()).await
                    }).await??;
                }
            }
        }
        Ok::<_, Box<dyn Error>>(())
    };

    tokio::try_join!(receive, respond)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::dns;
    use crate::resolver::echo::EchoResolver;

    #[tokio::test]
    async fn excess_connections_are_closed_and_capacity_is_reused() {
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let local = task::LocalSet::new();
        local
            .run_until(async {
                let server = task::spawn_local(start(
                    address,
                    std::rc::Rc::new(EchoResolver {}),
                    Arc::new(Semaphore::new(128)),
                ));
                tokio::task::yield_now().await;
                let mut clients = Vec::new();
                for _ in 0..MAX_CONNECTIONS {
                    clients.push(tokio::net::TcpStream::connect(address).await.unwrap());
                    tokio::task::yield_now().await;
                }
                let mut excess = tokio::net::TcpStream::connect(address).await.unwrap();
                assert_eq!(
                    timeout(Duration::from_secs(1), excess.read(&mut [0; 1]))
                        .await
                        .unwrap()
                        .unwrap(),
                    0,
                );
                drop(clients.pop());
                tokio::task::yield_now().await;
                let mut replacement = tokio::net::TcpStream::connect(address).await.unwrap();
                replacement.write_all(&[0, 2, 0, 1]).await.unwrap();
                assert_eq!(
                    timeout(Duration::from_secs(1), replacement.read_u16())
                        .await
                        .unwrap()
                        .unwrap(),
                    2
                );
                assert_eq!(replacement.read_u16().await.unwrap(), 1);
                server.abort();
            })
            .await;
    }

    async fn handle_connection<S, T>(socket: &mut S, resolver: &T) -> Result<(), Box<dyn Error>>
    where
        S: AsyncRead + AsyncWrite + Unpin,
        T: DnsResolver,
    {
        super::handle_connection(socket, resolver, Arc::new(Semaphore::new(128))).await
    }

    #[tokio::test(start_paused = true)]
    async fn global_capacity_wait_times_out() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        client.write_all(&[0, 2, 0, 1]).await.unwrap();
        client.shutdown().await.unwrap();
        let slots = Arc::new(Semaphore::new(0));
        let error = super::handle_connection(&mut socket, &EchoResolver {}, slots)
            .await
            .unwrap_err();
        assert!(error.is::<tokio::time::error::Elapsed>());
    }

    #[tokio::test(start_paused = true)]
    async fn connections_share_global_capacity_and_release_it() {
        let slots = Arc::new(Semaphore::new(1));
        let resolver = GatedResolver {
            active: std::sync::atomic::AtomicUsize::new(0),
            gate: Semaphore::new(0),
        };
        let (mut first, mut first_client) = tokio::io::duplex(64);
        let (mut second, mut second_client) = tokio::io::duplex(64);
        for client in [&mut first_client, &mut second_client] {
            client.write_all(&[0, 2, 0, 1]).await.unwrap();
            client.shutdown().await.unwrap();
        }
        let (first_result, second_result, _) = tokio::join!(
            super::handle_connection(&mut first, &resolver, slots.clone()),
            super::handle_connection(&mut second, &resolver, slots.clone()),
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                assert_eq!(resolver.active.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert_eq!(slots.available_permits(), 0);
                resolver.gate.add_permits(2);
            }
        );
        first_result.unwrap();
        second_result.unwrap();
        assert_eq!(resolver.active.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn resolver_failure_releases_global_capacity() {
        let slots = Arc::new(Semaphore::new(1));
        let (mut socket, mut client) = tokio::io::duplex(64);
        let query = dns_query();
        client.write_u16(query.len() as u16).await.unwrap();
        client.write_all(&query).await.unwrap();
        client.shutdown().await.unwrap();
        super::handle_connection(&mut socket, &StubResolver { response: None }, slots.clone())
            .await
            .unwrap();
        let response = read_dns_frame(&mut client).await;
        dns::validate_response(&query, &response).unwrap();
        assert_eq!(u16::from_be_bytes([response[2], response[3]]) & 0xf, 2);
        assert_eq!(slots.available_permits(), 1);
    }

    struct StubResolver {
        response: Option<Bytes>,
    }

    #[async_trait::async_trait]
    impl DnsResolver for StubResolver {
        async fn resolve(&self, _: Bytes) -> Result<Bytes, Box<dyn Error>> {
            match &self.response {
                Some(response) => Ok(response.clone()),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "upstream unavailable",
                )
                .into()),
            }
        }
    }

    #[tokio::test]
    async fn clean_disconnect_between_frames_succeeds() {
        let (mut socket, client) = tokio::io::duplex(64);
        drop(client);
        handle_connection(&mut socket, &EchoResolver {})
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn multiple_queries_reuse_connection() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        let (result, _) = tokio::join!(handle_connection(&mut socket, &EchoResolver {}), async {
            for query in [b"one", b"two", b"end"] {
                client.write_u16(3).await.unwrap();
                client.write_all(query).await.unwrap();
                assert_eq!(client.read_u16().await.unwrap(), 3);
                let mut response = [0; 3];
                client.read_exact(&mut response).await.unwrap();
                assert_eq!(&response, query);
            }
            client.shutdown().await.unwrap();
        });
        result.unwrap();
    }

    struct DelayedResolver;

    #[async_trait::async_trait]
    impl DnsResolver for DelayedResolver {
        async fn resolve(&self, query: Bytes) -> Result<Bytes, Box<dyn Error>> {
            if query[1] == 1 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Ok(query)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pipelined_queries_complete_out_of_order_after_half_close() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        let (result, _) = tokio::join!(handle_connection(&mut socket, &DelayedResolver), async {
            // Two frames sent together without waiting for either response.
            client.write_all(&[0, 2, 0, 1, 0, 2, 0, 2]).await.unwrap();
            client.shutdown().await.unwrap();
            let started = tokio::time::Instant::now();
            for id in [2, 1] {
                assert_eq!(client.read_u16().await.unwrap(), 2);
                assert_eq!(client.read_u16().await.unwrap(), id);
                if id == 2 {
                    assert_eq!(started.elapsed(), Duration::ZERO);
                }
            }
            assert_eq!(started.elapsed(), Duration::from_secs(1));
        });
        result.unwrap();
    }

    struct GatedResolver {
        active: std::sync::atomic::AtomicUsize,
        gate: Semaphore,
    }

    #[async_trait::async_trait]
    impl DnsResolver for GatedResolver {
        async fn resolve(&self, query: Bytes) -> Result<Bytes, Box<dyn Error>> {
            self.active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.gate.acquire().await?.forget();
            Ok(query)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pipeline_limits_outstanding_queries() {
        let (mut socket, mut client) = tokio::io::duplex(1024);
        let resolver = GatedResolver {
            active: std::sync::atomic::AtomicUsize::new(0),
            gate: Semaphore::new(0),
        };
        let (result, _) = tokio::join!(handle_connection(&mut socket, &resolver), async {
            for _ in 0..MAX_IN_FLIGHT + 1 {
                client.write_all(&[0, 2, 0, 1]).await.unwrap();
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert_eq!(
                resolver.active.load(std::sync::atomic::Ordering::SeqCst),
                MAX_IN_FLIGHT,
            );
            resolver.gate.add_permits(MAX_IN_FLIGHT + 1);
            for _ in 0..MAX_IN_FLIGHT + 1 {
                assert_eq!(client.read_u16().await.unwrap(), 2);
                assert_eq!(client.read_u16().await.unwrap(), 1);
            }
            client.shutdown().await.unwrap();
        });
        result.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn partial_next_frame_keeps_original_read_deadline() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        let started = tokio::time::Instant::now();
        let (result, _) = tokio::join!(handle_connection(&mut socket, &EchoResolver {}), async {
            client.write_all(&[0, 2, 0, 1]).await.unwrap();
            assert_eq!(client.read_u16().await.unwrap(), 2);
            assert_eq!(client.read_u16().await.unwrap(), 1);
            tokio::time::sleep(Duration::from_secs(4)).await;
            client.write_all(&[0]).await.unwrap();
        });
        assert!(result.unwrap_err().is::<tokio::time::error::Elapsed>());
        assert_eq!(started.elapsed(), IO_TIMEOUT);
    }

    #[tokio::test]
    async fn maximum_length_query_round_trips() {
        let (mut socket, mut client) = tokio::io::duplex(1024);
        let query = vec![42; usize::from(u16::MAX)];
        let (result, _) = tokio::join!(handle_connection(&mut socket, &EchoResolver {}), async {
            client.write_u16(u16::MAX).await.unwrap();
            client.write_all(&query).await.unwrap();
            assert_eq!(client.read_u16().await.unwrap(), u16::MAX);
            let mut response = vec![0; query.len()];
            client.read_exact(&mut response).await.unwrap();
            assert_eq!(response, query);
            client.shutdown().await.unwrap();
        });
        result.unwrap();
    }

    #[tokio::test]
    async fn incomplete_query_returns_eof() {
        // Disconnect during the prefix or during the query body.
        for partial_query in [&b"\x00"[..], &b"\x00\x03dn"[..]] {
            let (mut socket, mut client) = tokio::io::duplex(64);
            client.write_all(partial_query).await.unwrap();
            drop(client);
            let error = handle_connection(&mut socket, &EchoResolver {})
                .await
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::UnexpectedEof,
            );
        }
    }

    #[tokio::test]
    async fn disconnect_before_response_returns_write_error() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        client.write_u16(3).await.unwrap();
        client.write_all(b"dns").await.unwrap();
        drop(client);
        let error = handle_connection(&mut socket, &EchoResolver {})
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::BrokenPipe,
        );
    }

    #[tokio::test]
    async fn resolver_failure_returns_servfail_and_keeps_connection_open() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        let query = dns_query();
        let (result, _) = tokio::join!(
            handle_connection(&mut socket, &StubResolver { response: None }),
            async {
                for _ in 0..2 {
                    client.write_u16(query.len() as u16).await.unwrap();
                    client.write_all(&query).await.unwrap();
                }
                client.shutdown().await.unwrap();
                for _ in 0..2 {
                    let response = read_dns_frame(&mut client).await;
                    dns::validate_response(&query, &response).unwrap();
                    assert_eq!(u16::from_be_bytes([response[2], response[3]]) & 0xf, 2);
                }
            }
        );
        result.unwrap();
    }

    fn dns_query() -> Vec<u8> {
        b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x03www\x07example\x03com\x00\x00\x01\x00\x01".to_vec()
    }

    async fn read_dns_frame<S: AsyncRead + Unpin>(socket: &mut S) -> Vec<u8> {
        let length = usize::from(socket.read_u16().await.unwrap());
        let mut response = vec![0; length];
        socket.read_exact(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn oversized_response_is_rejected_before_writing() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        client.write_u16(3).await.unwrap();
        client.write_all(b"dns").await.unwrap();
        let resolver = StubResolver {
            response: Some(Bytes::from(vec![0; usize::from(u16::MAX) + 1])),
        };
        let error = handle_connection(&mut socket, &resolver).await.unwrap_err();
        assert!(error.is::<std::num::TryFromIntError>());
        drop(socket);
        assert_eq!(client.read(&mut [0; 1]).await.unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_connection_times_out() {
        let (mut socket, client) = tokio::io::duplex(64);
        let started = tokio::time::Instant::now();
        let error = handle_connection(&mut socket, &EchoResolver {})
            .await
            .unwrap_err();
        assert!(error.is::<tokio::time::error::Elapsed>());
        assert_eq!(started.elapsed(), IO_TIMEOUT);
        drop(client);
    }

    #[tokio::test(start_paused = true)]
    async fn query_read_has_one_deadline_for_header_and_body() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        let started = tokio::time::Instant::now();
        let (result, _) = tokio::join!(handle_connection(&mut socket, &EchoResolver {}), async {
            tokio::time::sleep(Duration::from_secs(4)).await;
            client.write_u16(16).await.unwrap();
            client.write_all(&[0]).await.unwrap();
        });
        assert!(result.unwrap_err().is::<tokio::time::error::Elapsed>());
        assert_eq!(started.elapsed(), IO_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_response_write_times_out() {
        let (mut socket, mut client) = tokio::io::duplex(8);
        let started = tokio::time::Instant::now();
        let (result, _) = tokio::join!(handle_connection(&mut socket, &EchoResolver {}), async {
            client.write_u16(8).await.unwrap();
            client.write_all(&[0; 8]).await.unwrap();
            // Keep the client open without consuming the response.
        });
        assert!(result.unwrap_err().is::<tokio::time::error::Elapsed>());
        assert_eq!(started.elapsed(), IO_TIMEOUT);
    }

    #[tokio::test]
    async fn query_completes_before_deadline() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        let (result, _) = tokio::join!(handle_connection(&mut socket, &EchoResolver {}), async {
            client.write_u16(3).await.unwrap();
            client.write_all(b"dns").await.unwrap();
            assert_eq!(client.read_u16().await.unwrap(), 3);
            let mut response = [0; 3];
            client.read_exact(&mut response).await.unwrap();
            assert_eq!(&response, b"dns");
            client.shutdown().await.unwrap();
        });
        result.unwrap();
    }
}
