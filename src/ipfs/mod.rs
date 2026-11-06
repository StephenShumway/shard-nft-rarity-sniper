//! IPFS metadata scraper.
//!
//! Fetches every token's metadata JSON through a set of HTTP gateways, with a
//! per-gateway concurrency cap, hedged requests with failover, and a coverage
//! cutoff so a reveal snipe doesn't wait on the slowest stragglers.

use anyhow::Result;
use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::info;

/// An IPFS HTTP gateway. `url` replaces the `ipfs://` scheme verbatim, so it
/// must include the path prefix and trailing slash (`https://<host>/ipfs/`).
/// `key`, if set, is sent as the `x-pinata-gateway-token` header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gateway {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribute {
    pub trait_type: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenMetadata {
    // Only `attributes` is consumed downstream by the rarity engine.
    // `name`/`description`/`image` are intentionally not deserialized: serde
    // silently skips unknown fields, saving 3 String allocations per token
    // across a full collection scrape.
    //
    // `alias = "traits"` accepts the older key a minority of projects still
    // use, without a per-collection code change.
    #[serde(alias = "traits")]
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone)]
pub struct TokenData {
    pub token_id: u64,
    pub metadata: TokenMetadata,
}

pub struct IpfsScraper {
    client: Client,
    gateways: Arc<[Gateway]>,
    /// One semaphore per gateway, index-aligned with `gateways`, so the
    /// concurrency cap is enforced *per gateway* rather than globally.
    semaphores: Arc<[Arc<Semaphore>]>,
    coverage_pct: u8,
}

impl IpfsScraper {
    /// Construct a scraper. `gateways` must be non-empty. Tokens are
    /// distributed across gateways by `token_id % gateways.len()`.
    /// `per_gateway_concurrency` is the max in-flight requests *each* gateway
    /// allows (total in-flight = this × gateway count). `coverage_pct` is the
    /// share of the supply that must be fetched before the scrape returns
    /// (1 to 100). Stragglers past that point are abandoned.
    pub fn new(gateways: Vec<Gateway>, per_gateway_concurrency: usize, coverage_pct: u8) -> Self {
        assert!(!gateways.is_empty(), "at least one gateway required");

        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(400)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_while_idle(true)
            .build()
            .expect("Failed to build HTTP client");

        let semaphores: Arc<[Arc<Semaphore>]> = gateways
            .iter()
            .map(|_| Arc::new(Semaphore::new(per_gateway_concurrency)))
            .collect();

        Self {
            client,
            gateways: Arc::from(gateways.into_boxed_slice()),
            semaphores,
            coverage_pct,
        }
    }

    /// Fire a small burst of throwaway requests at *each* gateway to fill the
    /// keep-alive connection pools before the scrape kicks off. Eliminates the
    /// TCP+TLS handshake cost from the first batch of real fetches.
    pub async fn prewarm(&self, count: usize) {
        let per_gateway = (count / self.gateways.len()).max(1);

        let mut handles = Vec::with_capacity(per_gateway * self.gateways.len());
        for gateway in self.gateways.iter() {
            let gateway_url = gateway.url.trim_end_matches('/').to_string();
            let key = gateway.key.clone();
            for _ in 0..per_gateway {
                let client = self.client.clone();
                let url = gateway_url.clone();
                let key = key.clone();
                handles.push(tokio::spawn(async move {
                    let mut req = client.get(&url);
                    if let Some(k) = key {
                        req = req.header("x-pinata-gateway-token", k);
                    }
                    let _ = req.send().await;
                }));
            }
        }
        let _ = futures::future::join_all(handles).await;
    }

    /// Scrape while reporting progress through `on_progress(fetched, target)`.
    /// `target` is the coverage-adjusted goal that, once reached, ends the
    /// scrape, so `fetched / target` runs cleanly 0→100%. Used by the desktop
    /// pipeline, which throttles the callback itself. Draws no stdout bar.
    pub async fn scrape_collection_with_progress<F>(
        &self,
        base_uri: &str,
        supply: u64,
        on_progress: F,
    ) -> Vec<TokenData>
    where
        F: Fn(u64, u64) + Send,
    {
        let extension = self.detect_extension(base_uri).await;
        self.scrape_per_token(base_uri, supply, &extension, on_progress)
            .await
    }

    /// Probe token 1 with no extension, then .json, to detect the format.
    /// Uses the first gateway because extension is a property of the collection,
    /// not of the gateway.
    async fn detect_extension(&self, base_uri: &str) -> String {
        let t = Instant::now();
        let probe_gateway = &self.gateways[0];

        // The `.json` fallback only fires if the plain probe misses, so `reqs`
        // tells us whether the extension cost one cold round-trip or two.
        let url_plain = resolve_url(probe_gateway, &build_token_uri(base_uri, 1, ""));
        let plain_ok = self.probe_url(probe_gateway, &url_plain).await;
        let (ext, reqs) = if plain_ok {
            (String::new(), 1)
        } else {
            let url_json = resolve_url(probe_gateway, &build_token_uri(base_uri, 1, ".json"));
            if self.probe_url(probe_gateway, &url_json).await {
                (".json".to_string(), 2)
            } else {
                (String::new(), 2)
            }
        };

        info!(
            "extension probe: {:?} in {:.2}s ({reqs} cold request(s) on gateway[0])",
            ext,
            t.elapsed().as_secs_f32(),
        );
        ext
    }

    async fn probe_url(&self, gateway: &Gateway, url: &str) -> bool {
        let mut req = self.client.get(url);
        if let Some(ref key) = gateway.key {
            req = req.header("x-pinata-gateway-token", key);
        }
        match req.send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Core token-fetch loop. `on_progress(fetched, target)` is invoked from the
    /// (single-threaded) join loop as successful fetches accumulate, plus once
    /// at the end (never from the spawned fetch tasks), so the caller's sink
    /// sees a monotonic count without cross-thread coordination.
    async fn scrape_per_token<F>(
        &self,
        base_uri: &str,
        supply: u64,
        extension: &str,
        on_progress: F,
    ) -> Vec<TokenData>
    where
        F: Fn(u64, u64) + Send,
    {
        let ext: Arc<str> = Arc::from(extension);

        // Proceed once `coverage_pct`% of the supply is fetched. Cold-fetch
        // stragglers form a long tail, and for a reveal snipe, waiting on the
        // slowest few percent can cost a whole block.
        let target_count = target_count(supply, self.coverage_pct);
        let target_u64 = target_count as u64;

        // Times only the fetch phase (the extension probe is logged separately).
        let scrape_start = Instant::now();
        let mut set: JoinSet<Result<(TokenData, u32)>> = JoinSet::new();
        for id in 1..=supply {
            let uri = build_token_uri(base_uri, id, &ext);
            let client = self.client.clone();
            let gateways = Arc::clone(&self.gateways);
            let semaphores = Arc::clone(&self.semaphores);
            set.spawn(async move {
                fetch_token_hedged(&client, &gateways, &semaphores, id, &uri).await
            });
        }

        // Coverage-curve checkpoints: time to the *first* token back (≈ cold
        // root resolution + one file pull) vs. to half vs. to the coverage cap.
        // A slow first token but fast 50→100% means root-resolution-bound
        // (warming the gateway's cache for the CID helps). A quick first token
        // but a long grind to target means per-file-bound (cache warming barely
        // helps). `retried` counts fetches won by a hedged or failed-over
        // gateway rather than the primary on the first try (the cold-straggler
        // signal).
        let half_mark = (target_count / 2).max(1);
        let mut first_token: Option<Duration> = None;
        let mut to_half: Option<Duration> = None;
        let mut retried = 0u64;

        let mut tokens: Vec<TokenData> = Vec::with_capacity(target_count);
        let mut failed = 0u64;
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok((data, attempts))) => {
                    if attempts > 1 {
                        retried += 1;
                    }
                    tokens.push(data);
                    let n = tokens.len();
                    if n == 1 {
                        first_token = Some(scrape_start.elapsed());
                    }
                    if n == half_mark {
                        to_half = Some(scrape_start.elapsed());
                    }
                    on_progress(n as u64, target_u64);
                    if n >= target_count {
                        break;
                    }
                }
                Ok(Err(_)) => failed += 1,
                Err(_) => failed += 1, // task panicked or was cancelled
            }
        }
        // Dropping the JoinSet aborts every still-running or still-queued fetch.
        drop(set);
        on_progress(tokens.len() as u64, target_u64); // final flush (100% if target reached)

        let abandoned = supply
            .saturating_sub(tokens.len() as u64)
            .saturating_sub(failed);
        info!(
            "Scrape coverage: {}/{} fetched ({} failed, {} abandoned at {}% cap)",
            tokens.len(),
            supply,
            failed,
            abandoned,
            self.coverage_pct
        );
        let secs = |d: Option<Duration>| d.map_or(f32::NAN, |d| d.as_secs_f32());
        info!(
            "Scrape timing: first token {:.2}s, 50% ({half_mark}) {:.2}s, target ({target_count}) {:.2}s — {retried} retried, {failed} failed",
            secs(first_token),
            secs(to_half),
            scrape_start.elapsed().as_secs_f32(),
        );

        tokens
    }
}

/// The coverage-adjusted number of successful fetches at which a scrape is
/// considered complete: `coverage_pct`% of `supply`, clamped to ≥1 so a tiny
/// pct can't bail before the first token.
fn target_count(supply: u64, coverage_pct: u8) -> usize {
    ((supply.saturating_mul(coverage_pct as u64)) / 100).max(1) as usize
}

/// How long a single gateway may run alone before we *hedge*: fire a parallel
/// copy of the request at the next gateway in the failover ring and take
/// whichever returns first. Set well below the client's 3s request timeout so a
/// cold straggler triggers a backup long before it would time out.
const HEDGE_DELAY: Duration = Duration::from_millis(800);

/// Max gateways a single token will try. With ≥3 gateways each attempt lands on
/// a distinct one (failover). With fewer, the ring wraps and re-hits.
const MAX_ATTEMPTS: usize = 3;

/// One fetch attempt against `gateways[gw]`, holding that gateway's semaphore
/// permit for the single HTTP round-trip only. It is released on drop the instant
/// the fetch resolves or the future is cancelled. `ordinal` is the 1-based position
/// in the failover ring, echoed back on success so the caller can tell a primary
/// hit (1) from a hedged/failed-over one (>1).
async fn fetch_attempt(
    client: &Client,
    gateways: &[Gateway],
    semaphores: &[Arc<Semaphore>],
    gw: usize,
    id: u64,
    uri: &str,
    ordinal: u32,
) -> Result<(TokenData, u32)> {
    let _permit = semaphores[gw].acquire().await.expect("semaphore closed");
    fetch_single(client, &gateways[gw], id, uri)
        .await
        .map(|data| (data, ordinal))
}

/// Fetch one token with **hedging + gateway failover**.
///
/// The first attempt goes to the token's primary gateway (`id % n`). If it
/// hasn't answered within [`HEDGE_DELAY`], a second attempt is launched *in
/// parallel* at the next gateway in the ring. The first success wins and the
/// rest are dropped (cancelling their in-flight requests and freeing their
/// permits). A hard error fails over to the next gateway immediately rather than
/// waiting out the hedge delay. Up to [`MAX_ATTEMPTS`] gateways are tried.
///
/// Each attempt holds only its own gateway's permit, and only for its own HTTP
/// round-trip (see [`fetch_attempt`]). A token being retried elsewhere doesn't
/// keep a slot pinned on the gateway that failed or stalled it, and losing
/// attempts free their slots as soon as one wins. Holding one permit across
/// sequential retries would throttle throughput.
///
/// Returns the token plus the 1-based ordinal of the gateway that won (1 =
/// primary on the first try, the warm common case, and >1 = hedged or failed-over,
/// the cold-straggler signal counted as "retried").
async fn fetch_token_hedged(
    client: &Client,
    gateways: &[Gateway],
    semaphores: &[Arc<Semaphore>],
    id: u64,
    uri: &str,
) -> Result<(TokenData, u32)> {
    let n = gateways.len();
    let base = id as usize % n;

    let mut in_flight = FuturesUnordered::new();
    in_flight.push(fetch_attempt(
        client, gateways, semaphores, base, id, uri, 1,
    ));
    let mut next = 1usize; // ring offset of the next gateway to launch

    loop {
        tokio::select! {
            biased;
            // React to a finished attempt before firing another hedge.
            Some(result) = in_flight.next() => match result {
                Ok(found) => return Ok(found),
                Err(e) => {
                    if next < MAX_ATTEMPTS {
                        // Hard failure → fail over to the next gateway now.
                        let gw = (base + next) % n;
                        in_flight.push(fetch_attempt(
                            client, gateways, semaphores, gw, id, uri, next as u32 + 1,
                        ));
                        next += 1;
                    } else if in_flight.is_empty() {
                        // Every gateway exhausted, so surface the last error.
                        return Err(e);
                    }
                    // Otherwise other hedged attempts are still running, so wait.
                }
            },
            // Primary is merely slow (not failed): hedge onto the next gateway.
            () = tokio::time::sleep(HEDGE_DELAY), if next < MAX_ATTEMPTS => {
                let gw = (base + next) % n;
                in_flight.push(fetch_attempt(
                    client, gateways, semaphores, gw, id, uri, next as u32 + 1,
                ));
                next += 1;
            }
        }
    }
}

async fn fetch_single(
    client: &Client,
    gateway: &Gateway,
    token_id: u64,
    uri: &str,
) -> Result<TokenData> {
    let url = resolve_url(gateway, uri);

    let mut req = client.get(&url);
    if let Some(ref key) = gateway.key {
        req = req.header("x-pinata-gateway-token", key);
    }

    let metadata = req.send().await?.json::<TokenMetadata>().await?;
    Ok(TokenData { token_id, metadata })
}

/// Resolve a URI to a full HTTP URL via the gateway. Only `ipfs://` URIs are
/// rewritten. Anything else (e.g. an `https://` base) is fetched as-is.
/// Pre-allocates exactly the needed capacity to avoid String regrowth on the hot path.
fn resolve_url(gateway: &Gateway, uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("ipfs://") {
        let mut out = String::with_capacity(gateway.url.len() + rest.len());
        out.push_str(&gateway.url);
        out.push_str(rest);
        out
    } else {
        uri.to_string()
    }
}

/// Build the per-token URI from the collection base, mirroring how an ERC-721
/// `tokenURI` concatenates the id. Three base shapes are handled:
///   * `{id}` template (`…/{id}.json`) → substitute `{id}` with `<id><ext>`.
///   * query-style base (`…/?`, `…?id=`, `…&n=`) → append `<id><ext>` verbatim.
///     The `?`/`=`/`&`/`#` already separates the id. Inserting a slash would
///     corrupt the query (`…/?/1`). This covers reveals whose `tokenURI` is
///     `concat(baseURI, id)` over a query-string base.
///   * directory base (`…/`, or no trailing separator) → normalise to exactly
///     one `/`, then append `<id><ext>`, giving `dir/1`.
///
/// Buffers are pre-sized (a u64 id is at most 20 decimal digits) so the
/// append paths never reallocate.
fn build_token_uri(base_uri: &str, id: u64, ext: &str) -> String {
    if base_uri.contains("{id}") {
        // Rare path, so allocate once via replace.
        let mut id_buf = String::with_capacity(20 + ext.len());
        // `{id}` is substituted in decimal. The ERC-1155 `{id}` convention is
        // 64-char zero-padded lowercase hex, which this would get wrong,
        // but that case is unlikely for an ERC-721 reveal.
        let _ = write!(id_buf, "{id}{ext}");
        return base_uri.replace("{id}", &id_buf);
    }

    // A base ending in a query separator gets the id concatenated directly.
    // Anything else is treated as a directory and gets a single `/` separator.
    if base_uri.ends_with(['?', '=', '&', '#']) {
        let mut out = String::with_capacity(base_uri.len() + 20 + ext.len());
        out.push_str(base_uri);
        let _ = write!(out, "{id}{ext}");
        return out;
    }

    let trimmed = base_uri.trim_end_matches('/');
    let mut out = String::with_capacity(trimmed.len() + 1 + 20 + ext.len());
    out.push_str(trimmed);
    out.push('/');
    let _ = write!(out, "{id}{ext}");
    out
}
