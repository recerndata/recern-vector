use clap::Parser;
use recern_vector_server::{
    http::{self, App},
    store::{Limits, Store},
};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "Single-node Recern Vector HTTP server (development milestone)"
)]
struct Args {
    #[arg(long)]
    database: PathBuf,
    #[arg(long)]
    api_key_file: PathBuf,
    #[arg(long, default_value = "127.0.0.1:7337")]
    listen: SocketAddr,
    #[arg(long, default_value_t = 67108864)]
    max_request_bytes: usize,
    #[arg(long, default_value_t = 67108864)]
    max_response_bytes: usize,
    #[arg(long, default_value_t = 8)]
    active_operations: usize,
    #[arg(long, default_value_t = 32)]
    queued_operations: usize,
    #[arg(long, default_value_t = 1)]
    max_snapshots: usize,
    #[arg(long, default_value_t = 300000)]
    snapshot_ttl_ms: u64,
    #[arg(long, default_value_t = 10000)]
    request_timeout_ms: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if !args.listen.ip().is_loopback() {
        fail("Only loopback listeners are supported; deploy a verified TLS reverse proxy.");
    }
    let key =
        std::fs::read(&args.api_key_file).unwrap_or_else(|_| fail("Cannot read the API key file."));
    if key.len() > 258 {
        fail("Invalid API key file.");
    }
    let key = key
        .strip_suffix(b"\r\n")
        .or_else(|| key.strip_suffix(b"\n"))
        .unwrap_or(&key);
    let limits = Limits {
        request: args.max_request_bytes,
        response: args.max_response_bytes,
        active: args.active_operations,
        queued: args.queued_operations,
        snapshots: args.max_snapshots,
        ttl_ms: args.snapshot_ttl_ms,
        default_timeout_ms: args.request_timeout_ms,
        ..Limits::default()
    };
    let store = Store::open(&args.database).unwrap_or_else(|e| fail(e.reason));
    let app = App::new(store, key, limits).unwrap_or_else(|e| fail(e.reason));
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .unwrap_or_else(|_| fail("Cannot bind the listener."));
    eprintln!(
        "Recern Vector server {} listening on {}",
        env!("CARGO_PKG_VERSION"),
        listener.local_addr().unwrap()
    );
    if http::serve(listener, app, shutdown()).await.is_err() {
        fail("The listener stopped unexpectedly.");
    }
}
fn fail(message: &str) -> ! {
    eprintln!("Recern Vector server: {message}");
    std::process::exit(1)
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
