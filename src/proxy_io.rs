use anyhow::Result;
use log::debug;
use tokio::io::{self, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn pump(stream: TcpStream) -> Result<()> {
    let (reader, writer) = stream.into_split();
    pump_io(io::stdin(), io::stdout(), reader, writer).await
}

async fn pump_io<I, O, R, W>(
    mut input: I,
    mut output: O,
    mut remote_reader: R,
    mut remote_writer: W,
) -> Result<()>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let to_remote = async {
        let copied = tokio::io::copy(&mut input, &mut remote_writer).await?;
        remote_writer.shutdown().await?;
        Ok::<_, anyhow::Error>(copied)
    };

    let from_remote = async {
        let copied = tokio::io::copy(&mut remote_reader, &mut output).await?;
        output.flush().await?;
        Ok::<_, anyhow::Error>(copied)
    };

    tokio::pin!(to_remote);
    tokio::pin!(from_remote);

    tokio::select! {
        result = &mut to_remote => {
            let bytes = result?;
            debug!("[sshpod][proxy_io] bytes_to_remote={}", bytes);
        }
        result = &mut from_remote => {
            let bytes = result?;
            debug!("[sshpod][proxy_io] bytes_from_remote={}", bytes);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::pump_io;
    use tokio::io::{self, AsyncReadExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn remote_eof_finishes_even_when_local_input_stays_open() {
        let (_local_writer, local_reader) = io::duplex(64);

        let result = timeout(
            Duration::from_millis(100),
            pump_io(local_reader, io::sink(), io::empty(), io::sink()),
        )
        .await;

        assert!(result.is_ok(), "pump_io should not wait for local input");
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn local_eof_shuts_down_tcp_write_half() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client = tokio::spawn(TcpStream::connect(addr));
        let (mut server, _) = listener.accept().await.unwrap();
        let client = client.await.unwrap().unwrap();
        let (remote_reader, remote_writer) = client.into_split();

        pump_io(io::empty(), io::sink(), remote_reader, remote_writer)
            .await
            .unwrap();

        let mut buf = [0; 1];
        let read = timeout(Duration::from_millis(100), server.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, 0);
    }
}
