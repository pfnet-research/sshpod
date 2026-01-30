use anyhow::{anyhow, Context, Result};
use log::debug;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

pub struct PortForward {
    child: tokio::process::Child,
    stdout_task: Option<JoinHandle<Result<()>>>,
    stderr_task: Option<JoinHandle<Result<()>>>,
}

impl PortForward {
    pub async fn start(
        context: Option<&str>,
        namespace: &str,
        pod: &str,
        remote_port: u16,
    ) -> Result<(PortForward, u16)> {
        let mut cmd = Command::new("kubectl");
        if let Some(ctx) = context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.args([
            "port-forward",
            "--address",
            "localhost",
            "-n",
            namespace,
            &format!("pod/{}", pod),
            &format!(":{}", remote_port),
        ]);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .context("failed to spawn kubectl port-forward process")?;

        let stdout = child
            .stdout
            .take()
            .context("failed to capture port-forward stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture port-forward stderr")?;

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let port = timeout(Duration::from_secs(10), async {
            loop {
                tokio::select! {
                    line = stdout_reader.next_line() => {
                        match line.context("failed to read port-forward stdout")? {
                            Some(l) => {
                                if let Some(port) = parse_port(&l) {
                                    debug!("[port-forward] {}", l);
                                    break Ok(port);
                                }
                                debug!("[port-forward] {}", l);
                            }
                            None => break Err(anyhow!("kubectl port-forward exited before reporting a port")),
                        }
                    }
                    line = stderr_reader.next_line() => {
                        if let Some(l) = line.context("failed to read port-forward stderr")? {
                            debug!("[port-forward] {}", l)
                        }
                    }
                    status = child.wait() => {
                        let status = status.context("failed to wait for port-forward process")?;
                        break Err(anyhow!("kubectl port-forward exited early with status {}", status));
                    }
                }
            }
        })
        .await
        .context("timed out waiting for port-forward to assign a local port")??;

        let stdout_task = tokio::spawn(async move {
            while let Some(line) = stdout_reader.next_line().await? {
                debug!("[port-forward] {}", line);
            }
            Ok::<_, anyhow::Error>(())
        });
        let stderr_task = tokio::spawn(async move {
            while let Some(line) = stderr_reader.next_line().await? {
                debug!("[port-forward] {}", line);
            }
            Ok::<_, anyhow::Error>(())
        });

        Ok((
            PortForward {
                child,
                stdout_task: Some(stdout_task),
                stderr_task: Some(stderr_task),
            },
            port,
        ))
    }

    pub async fn stop(&mut self) -> Result<()> {
        if self.child.id().is_some() {
            let _ = self.child.start_kill();
        }
        let _ = self.child.wait().await;

        if let Some(handle) = self.stdout_task.take() {
            let _ = handle.await;
        }
        if let Some(handle) = self.stderr_task.take() {
            let _ = handle.await;
        }
        Ok(())
    }
}

fn parse_port(line: &str) -> Option<u16> {
    if !line.contains("Forwarding from") {
        return None;
    }
    let token = line.split_whitespace().find(|p| p.contains(':'))?;
    let (_, port_str) = token.rsplit_once(':')?;
    port_str.parse().ok()
}
