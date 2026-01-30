mod bundle;
mod cli;
mod embedded;
mod hostspec;
mod install;
mod keys;
mod kubectl;
mod paths;
mod port_forward;
mod proxy;
mod proxy_io;
mod remote;

#[tokio::main]
async fn main() {
    if let Err(err) = cli::run().await {
        eprintln!("error: {:#}", err);
        std::process::exit(1);
    }
}
