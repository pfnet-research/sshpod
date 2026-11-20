use crate::{install, proxy};
use anyhow::{anyhow, Result};
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "sshpod",
    version,
    about = "ProxyCommand helper for ssh/scp/sftp to Kubernetes Pods"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// ProxyCommand entry point
    Proxy(ProxyArgs),
    /// Update ~/.ssh/config with the sshpod ProxyCommand block
    Configure,
}

#[derive(Args, Debug, Clone)]
pub struct ProxyArgs {
    /// Target host (e.g. api-xxxx.ns.sshpod)
    #[arg(long)]
    pub host: String,
    /// SSH login user (defaults to local user)
    #[arg(long)]
    pub user: Option<String>,
    /// OpenSSH-supplied port (unused but accepted for compatibility)
    #[arg(long)]
    pub port: Option<u16>,
    /// Log level: error, info, debug
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Proxy(args)) => proxy::run(args).await?,
        Some(Commands::Configure) => install::run().await?,
        None => {
            return Err(anyhow!(
                "no command provided. Use the configure or proxy subcommands."
            ))
        }
    }
    Ok(())
}
