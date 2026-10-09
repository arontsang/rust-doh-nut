use crate::resolver::DnsResolver;
use bytes::Bytes;
use std::convert::TryFrom;
use std::error::Error;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task;
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

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
    let payload = timeout(IO_TIMEOUT, async {
        let length = usize::from(socket.read_u16().await?);
        let mut buffer = vec![0u8; length];
        socket.read_exact(&mut buffer).await?;
        Ok::<_, std::io::Error>(Bytes::from(buffer))
    })
    .await??;

    let response = resolver.resolve(payload).await?;
    let length = u16::try_from(response.len())?;
    timeout(IO_TIMEOUT, async {
        socket.write_u16(length).await?;
        socket.write_all(response.as_ref()).await
    })
    .await??;
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
        });
        result.unwrap();
    }

    #[tokio::test]
    async fn incomplete_query_returns_eof() {
        // Disconnect before the prefix, during it, or during the query body.
        for partial_query in [&b""[..], &b"\x00"[..], &b"\x00\x03dn"[..]] {
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
        });
        result.unwrap();
    }
}
