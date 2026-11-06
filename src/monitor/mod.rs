//! Shared background monitor.
//!
//! One [`MonitorHub`] is created at app start. It owns **one** persistent
//! bloxroute WebSocket connection and **one** QuickNode webhook server, shared
//! by every running task. Because bloxroute filters server-side by `from`, the
//! hub keeps a refcounted set of watched deployer addresses and issues **one
//! subscription per distinct deployer** over the single connection, so a
//! second task with a different deployer just adds a subscription, and the
//! connection never has to close/reopen. The QuickNode webhook matches incoming
//! block txs against the same deployer set.
//!
//! Every reveal candidate (from either source) is published on a
//! [`broadcast`] channel as a [`CandidateTx`]. Each task subscribes, filters by
//! its own deployer, decodes with its own reveal selector, and runs
//! its pipeline. The hub is lazy: it connects on the first
//! [`register`](MonitorHub::register) and tears everything down when the last
//! deployer is [`deregister`](MonitorHub::deregister)ed, so an idle app holds
//! no bloxroute connection and no listening port.

use alloy::primitives::Address;
use anyhow::{anyhow, Context, Result};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use chrono::Utc;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::mempool::{
    build_subscribe_message, build_unsubscribe_message, build_ws_request, candidate_from_parts,
    parse_pending_tx_notification, parse_subscription_ack, CandidateTx,
};
use crate::settings::Settings;
use crate::streamwatcher::{decode_body, parse_stream_blocks, verify_signature};

const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(3);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);
const CANDIDATE_CHANNEL_CAP: usize = 1024;
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Incremental subscription control sent to the live bloxroute task.
enum BloxCmd {
    Subscribe(Address),
    Unsubscribe(Address),
}

/// Global monitor debug log: `logs/monitor/<date>.debug.log`, dated (UTC) when
/// the hub is created. Records the stream-wide signals that aren't owned by any
/// single task: every block number received from the QuickNode stream and every
/// candidate tx seen on the bloxroute connection. Saved to disk only (never
/// shown in the UI). Per-task detail lives in each task's own debug log.
struct MonitorDebug {
    path: PathBuf,
    lock: Mutex<()>,
}

impl MonitorDebug {
    fn new() -> Arc<Self> {
        let dir = std::path::Path::new("logs").join("monitor");
        let _ = std::fs::create_dir_all(&dir);
        let date = Utc::now().format("%Y-%m-%d");
        Arc::new(Self {
            path: dir.join(format!("{date}.debug.log")),
            lock: Mutex::new(()),
        })
    }

    fn log(&self, line: &str) {
        let ts = Utc::now().to_rfc3339();
        let Ok(_guard) = self.lock.lock() else {
            return;
        };
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{ts}  {line}");
        }
    }
}

/// Mutable lifecycle state, guarded by a (non-async) mutex. We never hold the
/// guard across an `.await`: all control sends use an unbounded channel.
#[derive(Default)]
struct HubRuntime {
    running: bool,
    cancel: Option<CancellationToken>,
    blox_cmd: Option<mpsc::UnboundedSender<BloxCmd>>,
    handles: Vec<JoinHandle<()>>,
}

pub struct MonitorHub {
    settings: Arc<RwLock<Settings>>,
    /// Refcount of watched deployers → number of running tasks watching each.
    deployers: Arc<DashMap<Address, usize>>,
    candidate_tx: broadcast::Sender<CandidateTx>,
    debug: Arc<MonitorDebug>,
    inner: Mutex<HubRuntime>,
}

impl MonitorHub {
    #[must_use]
    pub fn new(settings: Arc<RwLock<Settings>>) -> Self {
        let (candidate_tx, _) = broadcast::channel(CANDIDATE_CHANNEL_CAP);
        Self {
            settings,
            deployers: Arc::new(DashMap::new()),
            candidate_tx,
            debug: MonitorDebug::new(),
            inner: Mutex::new(HubRuntime::default()),
        }
    }

    /// A receiver for the shared reveal-candidate stream. Each task takes one.
    ///
    /// Take it *before* calling [`register`](Self::register): a broadcast
    /// receiver only sees messages sent after it was created, so subscribing
    /// afterwards could miss the first candidate.
    #[must_use]
    pub fn subscribe_candidates(&self) -> broadcast::Receiver<CandidateTx> {
        self.candidate_tx.subscribe()
    }

    /// Start watching `deployer` (refcounted). Starts the hub on the first
    /// deployer and adds a bloxroute subscription when a new deployer appears.
    ///
    /// # Errors
    /// Currently infallible in practice, but returns `Result` for forward-compat.
    pub async fn register(&self, deployer: Address) -> Result<()> {
        let (newly_added, watchers) = {
            let mut c = self.deployers.entry(deployer).or_insert(0);
            *c += 1;
            (*c == 1, *c)
        };

        // Snapshot settings before locking: no `.await` while the std mutex is held.
        let settings = self.settings.read().await.clone();

        let mut rt = self.inner.lock().expect("hub mutex poisoned");
        if !rt.running {
            self.start_locked(&mut rt, &settings);
            info!("MonitorHub started (deployer {deployer:#x})");
        } else if newly_added {
            if let Some(tx) = &rt.blox_cmd {
                let _ = tx.send(BloxCmd::Subscribe(deployer));
            }
            info!("MonitorHub: added subscription for {deployer:#x}");
        } else {
            info!("MonitorHub: deployer {deployer:#x} now watched by {watchers} task(s) (shared subscription)");
        }
        Ok(())
    }

    /// Stop watching `deployer` (refcounted). Removes its bloxroute subscription
    /// when the last task on it stops, and tears the hub down entirely when no
    /// deployers remain.
    pub async fn deregister(&self, deployer: Address) {
        let now_zero = match self.deployers.get_mut(&deployer) {
            Some(mut c) => {
                let v = c.saturating_sub(1);
                *c = v;
                v == 0
            }
            None => false,
        };
        if now_zero {
            self.deployers.remove(&deployer);
        }

        let mut rt = self.inner.lock().expect("hub mutex poisoned");
        if rt.running && now_zero {
            if let Some(tx) = &rt.blox_cmd {
                let _ = tx.send(BloxCmd::Unsubscribe(deployer));
            }
        }
        if self.deployers.is_empty() && rt.running {
            self.stop_locked(&mut rt);
            info!("MonitorHub stopped (no deployers left)");
        }
    }

    /// Spawn the bloxroute and webhook tasks. Their connection settings are
    /// captured here, so edits to them take effect on the next start (after
    /// every task has stopped).
    fn start_locked(&self, rt: &mut HubRuntime, settings: &Settings) {
        let cancel = CancellationToken::new();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let blox = tokio::spawn(run_bloxroute(
            settings.blox_stream_url.clone(),
            settings.blox_auth_header.clone(),
            Arc::clone(&self.deployers),
            self.candidate_tx.clone(),
            Arc::clone(&self.debug),
            cmd_rx,
            cancel.clone(),
        ));

        let token = settings.qn_security_token.clone().filter(|t| !t.is_empty());
        let webhook = tokio::spawn(run_webhook(
            settings.stream_port,
            token,
            Arc::clone(&self.deployers),
            self.candidate_tx.clone(),
            Arc::clone(&self.debug),
            cancel.clone(),
        ));

        rt.running = true;
        rt.cancel = Some(cancel);
        rt.blox_cmd = Some(cmd_tx);
        rt.handles = vec![blox, webhook];
    }

    fn stop_locked(&self, rt: &mut HubRuntime) {
        if let Some(c) = rt.cancel.take() {
            c.cancel();
        }
        rt.blox_cmd = None;
        // Tasks shut down on cancel (graceful), so we detach rather than await.
        rt.handles.clear();
        rt.running = false;
    }
}

// ── bloxroute side ──────────────────────────────────────────────────────────

/// Owns the single bloxroute WS connection for the hub's lifetime. Reconnects
/// with backoff, re-subscribing all active deployers on each (re)connection.
async fn run_bloxroute(
    url: String,
    auth: String,
    deployers: Arc<DashMap<Address, usize>>,
    candidate_tx: broadcast::Sender<CandidateTx>,
    debug_log: Arc<MonitorDebug>,
    mut cmd_rx: mpsc::UnboundedReceiver<BloxCmd>,
    cancel: CancellationToken,
) {
    if url.is_empty() {
        warn!("MonitorHub: bloxroute url not configured — mempool monitor disabled");
        return;
    }
    let mut backoff = INITIAL_RECONNECT_BACKOFF;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        match blox_connect_run(
            &url,
            &auth,
            &deployers,
            &candidate_tx,
            &debug_log,
            &mut cmd_rx,
            &cancel,
        )
        .await
        {
            Ok(true) => return, // cancelled, stop for good
            Ok(false) => {
                debug!("bloxroute stream closed, reconnecting...");
                backoff = INITIAL_RECONNECT_BACKOFF;
            }
            Err(e) => warn!("bloxroute monitor error: {e} — reconnecting in {backoff:?}"),
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_RECONNECT_BACKOFF);
    }
}

/// One connection lifetime. Returns `Ok(true)` if cancelled (stop), `Ok(false)`
/// on a clean server close (reconnect), or `Err` on a connection error.
async fn blox_connect_run(
    url: &str,
    auth: &str,
    deployers: &DashMap<Address, usize>,
    candidate_tx: &broadcast::Sender<CandidateTx>,
    debug_log: &MonitorDebug,
    cmd_rx: &mut mpsc::UnboundedReceiver<BloxCmd>,
    cancel: &CancellationToken,
) -> Result<bool> {
    let request = build_ws_request(url, auth)?;
    let (ws_stream, _) = connect_async(request)
        .await
        .context("bloxroute WS handshake failed")?;
    let (mut write, mut read) = ws_stream.split();

    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, Address> = HashMap::new(); // req id → addr awaiting ack
    let mut addr_sub: HashMap<Address, String> = HashMap::new(); // addr → subscription id

    // (Re)subscribe to every currently-watched deployer.
    for entry in deployers.iter() {
        let addr = *entry.key();
        let id = next_id;
        next_id += 1;
        pending.insert(id, addr);
        write
            .send(Message::Text(build_subscribe_message(&addr, id)))
            .await
            .context("send subscribe failed")?;
    }
    debug!("bloxroute: (re)subscribed {} deployer(s)", pending.len());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(true),

            cmd = cmd_rx.recv() => match cmd {
                Some(BloxCmd::Subscribe(addr)) => {
                    if addr_sub.contains_key(&addr) || pending.values().any(|a| *a == addr) {
                        continue;
                    }
                    let id = next_id;
                    next_id += 1;
                    pending.insert(id, addr);
                    write.send(Message::Text(build_subscribe_message(&addr, id)))
                        .await.context("send subscribe failed")?;
                }
                Some(BloxCmd::Unsubscribe(addr)) => {
                    if let Some(sub) = addr_sub.remove(&addr) {
                        let id = next_id;
                        next_id += 1;
                        write.send(Message::Text(build_unsubscribe_message(&sub, id)))
                            .await.context("send unsubscribe failed")?;
                    }
                    pending.retain(|_, a| *a != addr);
                }
                None => return Ok(true), // hub stopped (cmd channel dropped)
            },

            msg = read.next() => {
                let Some(msg) = msg else { return Ok(false) };
                let text = match msg {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) => return Ok(false),
                    Ok(_) => continue,
                    Err(e) => return Err(anyhow!("websocket read error: {e}")),
                };

                if let Some((id, sub)) = parse_subscription_ack(&text) {
                    if let Some(addr) = pending.remove(&id) {
                        addr_sub.insert(addr, sub);
                    }
                    continue;
                }
                if let Some((from, hash, input)) = parse_pending_tx_notification(&text) {
                    debug_log.log(&format!("bloxroute pending tx {hash} from {from}"));
                    if let Some(cand) = candidate_from_parts(&from, &hash, &input) {
                        let _ = candidate_tx.send(cand);
                    }
                }
            }
        }
    }
}

// ── QuickNode webhook side ──────────────────────────────────────────────────

struct WebhookState {
    deployers: Arc<DashMap<Address, usize>>,
    security_token: Option<String>,
    candidate_tx: broadcast::Sender<CandidateTx>,
    debug_log: Arc<MonitorDebug>,
}

/// Binds the QuickNode webhook once and serves until cancelled. Matches each
/// pushed tx's `from` against the active deployer set and broadcasts matches.
async fn run_webhook(
    port: u16,
    security_token: Option<String>,
    deployers: Arc<DashMap<Address, usize>>,
    candidate_tx: broadcast::Sender<CandidateTx>,
    debug_log: Arc<MonitorDebug>,
    cancel: CancellationToken,
) {
    let state = Arc::new(WebhookState {
        deployers,
        security_token,
        candidate_tx,
        debug_log,
    });
    let app = Router::new()
        .route("/", post(webhook_handler))
        .route("/webhook", post(webhook_handler))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!("MonitorHub: failed to bind webhook on {addr}: {e}");
            return;
        }
    };
    if state.security_token.is_some() {
        debug!("MonitorHub webhook listening on http://{addr}/webhook (HMAC on)");
    } else {
        warn!("MonitorHub webhook on http://{addr}/webhook is UNAUTHENTICATED (no QN token set)");
    }

    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await;
}

async fn webhook_handler(
    State(state): State<Arc<WebhookState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let payload = match decode_body(&headers, &body) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if let Some(token) = &state.security_token {
        if verify_signature(token, &headers, &payload).is_err() {
            return StatusCode::UNAUTHORIZED;
        }
    }

    if let Ok(blocks) = parse_stream_blocks(&payload) {
        for block in blocks {
            match block.number {
                Some(n) => state
                    .debug_log
                    .log(&format!("stream block {n} ({} txs)", block.txs.len())),
                None => state.debug_log.log(&format!(
                    "stream block (no number, {} txs)",
                    block.txs.len()
                )),
            }
            for (from, hash, input) in block.txs {
                let Ok(addr) = from.parse::<Address>() else {
                    continue;
                };
                if !state.deployers.contains_key(&addr) {
                    continue;
                }
                state
                    .debug_log
                    .log(&format!("stream candidate tx {hash} from {from}"));
                if let Some(cand) = candidate_from_parts(&from, &hash, &input) {
                    let _ = state.candidate_tx.send(cand);
                }
            }
        }
    }
    StatusCode::OK
}
