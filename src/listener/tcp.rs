use crate::resolver::DnsResolver;
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

pub async fn start<T: DnsResolver + 'static>(
    bind_address: SocketAddr,
    resolver: std::rc::Rc<T>,
) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(bind_address).await?;
    loop {
        let (mut socket, _) = listener.accept().await?;
        task::spawn_local({
            let resolver = resolver.clone();
            async move {
                if let Err(error) = handle_connection(&mut socket, resolver.as_ref()).await {
                    eprintln!("Error handling TCP query: {}", error);
                }
            }
        });
    }
}

async fn handle_connection<S, T>(socket: &mut S, resolver: &T) -> Result<(), Box<dyn Error>>
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
                        Some((query, slot)) => pending.push(async move {
                            let response = resolver.resolve(query).await?;
                            Ok::<_, Box<dyn Error>>((response, slot))
                        }),
                        None => input_open = false,
                    }
                }
                Some(result) = pending.next(), if !pending.is_empty() => {
                    // Hold the slot until the complete response frame is written.
                    let (response, _slot) = result?;
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
    use crate::resolver::echo::EchoResolver;

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
    async fn resolver_failure_returns_error_without_response() {
        let (mut socket, mut client) = tokio::io::duplex(64);
        client.write_u16(3).await.unwrap();
        client.write_all(b"dns").await.unwrap();
        let error = handle_connection(&mut socket, &StubResolver { response: None })
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::ConnectionRefused,
        );
        drop(socket);
        assert_eq!(client.read(&mut [0; 1]).await.unwrap(), 0);
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
