//! One-shot endpoint reachability + RPC latency probes for the Settings "test"
//! buttons. Endpoint pings are a timed TCP connect (works for `ws(s)://`,
//! `http(s)://`, and relay URLs alike, and sidesteps per-endpoint auth). The RPC
//! probe connects a provider and times a `get_block_number` call.

use alloy::providers::{Provider, ProviderBuilder};
use anyhow::{Context, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

const PING_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Serialize)]
pub struct PingResult {
    pub ok: bool,
    pub ms: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcResult {
    pub ok: bool,
    pub ms: u64,
    pub block: u64,
    pub detail: String,
}

/// Timed TCP connect to the URL's host:port. Reachable + latency, no auth.
pub async fn ping_endpoint(url: &str) -> PingResult {
    let started = Instant::now();
    match tokio::time::timeout(PING_TIMEOUT, tcp_connect(url)).await {
        Ok(Ok(())) => PingResult {
            ok: true,
            ms: started.elapsed().as_millis() as u64,
            detail: "reachable".to_string(),
        },
        Ok(Err(e)) => PingResult {
            ok: false,
            ms: started.elapsed().as_millis() as u64,
            detail: e.to_string(),
        },
        Err(_) => PingResult {
            ok: false,
            ms: PING_TIMEOUT.as_millis() as u64,
            detail: "timed out".to_string(),
        },
    }
}

async fn tcp_connect(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).context("invalid url")?;
    let host = parsed.host_str().context("url has no host")?;
    let port = parsed
        .port_or_known_default()
        .context("url has no port and an unknown scheme")?;
    tokio::net::TcpStream::connect((host, port))
        .await
        .context("tcp connect failed")?;
    Ok(())
}

/// Connect to the JSON-RPC endpoint and time a `get_block_number` round trip.
pub async fn rpc_latency(url: &str) -> RpcResult {
    match measure_rpc(url).await {
        Ok((ms, block)) => RpcResult {
            ok: true,
            ms,
            block,
            detail: format!("block {block}"),
        },
        Err(e) => RpcResult {
            ok: false,
            ms: 0,
            block: 0,
            detail: e.to_string(),
        },
    }
}

async fn measure_rpc(url: &str) -> Result<(u64, u64)> {
    let provider = ProviderBuilder::new()
        .connect(url)
        .await
        .context("RPC connect failed")?;
    let start = Instant::now();
    let block = provider
        .get_block_number()
        .await
        .context("get_block_number failed")?;
    Ok((start.elapsed().as_millis() as u64, block))
}
