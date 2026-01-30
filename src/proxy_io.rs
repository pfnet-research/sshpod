use anyhow::Result;
use tokio::io::{self, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn pump(stream: TcpStream) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    let to_remote = tokio::spawn(async move {
        let copied = tokio::io::copy(&mut stdin, &mut writer).await?;
        writer.shutdown().await?;
        Ok::<_, anyhow::Error>(copied)
    });

    let from_remote = tokio::spawn(async move {
        let copied = tokio::io::copy(&mut reader, &mut stdout).await?;
        stdout.flush().await?;
        Ok::<_, anyhow::Error>(copied)
    });

    let (a, b) = tokio::join!(to_remote, from_remote);
    let to_bytes = a??;
    let from_bytes = b??;
    // Debug logging kept compact
    eprintln!(
        "[sshpod][proxy_io] bytes_to_remote={} bytes_from_remote={}",
        to_bytes, from_bytes
    );
    Ok(())
}
