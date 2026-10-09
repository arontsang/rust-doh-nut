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
