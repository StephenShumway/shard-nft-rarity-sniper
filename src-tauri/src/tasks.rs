//! Task start + run loop: subscribe to the shared monitor, filter for this
//! task's deployer, decode with its reveal selector, and run the reveal
//! pipeline. Stopping cancels the loop, which deregisters from the hub.

use std::collections::HashSet;
use std::sync::Arc;

use alloy::primitives::Address;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use rarity_sniper::mempool::{decode_reveal_uri, DecodeKind, RevealEvent};
use rarity_sniper::monitor::MonitorHub;
use rarity_sniper::pipeline::{run_reveal_pipeline, TaskContext};
use rarity_sniper::reporter::TaskReporter;
use rarity_sniper::task::{self, TaskConfig, TaskStage};

use crate::state::AppState;

/// Start a task by slug: resolve its wallet, build its context, and spawn the
/// monitor loop. Returns an error string suitable for surfacing in the UI.
pub async fn start(state: &AppState, slug: &str) -> Result<(), String> {
    if state.tasks.is_running(slug) {
        return Err(format!("task {slug} is already running"));
    }

    let cfg = task::load(slug).map_err(|e| e.to_string())?;
    let deployer: Address = cfg
        .deployer
        .parse()
        .map_err(|_| format!("invalid deployer address {:?}", cfg.deployer))?;

    // Validate the reveal selector before connecting anything, so a blank or
    // malformed one stops the task with a clear error instead of silently
    // rejecting every candidate tx as "not a reveal".
    let reveal = &cfg.reveal_selector;
    let selector = reveal
        .selector_bytes()
        .map_err(|e| format!("invalid reveal selector {:?}: {e}", reveal.selector))?;
    let kind = reveal.kind;

    // Resolve the buying wallet's key from the store.
    let private_key = {
        let wallets = state.wallets.read().await;
        wallets
            .find(&cfg.wallet)
            .map(|w| w.private_key.clone())
            .ok_or_else(|| format!("buying wallet {} not found in wallet store", cfg.wallet))?
    };

    let settings = state.settings.read().await.clone();
    let reporter = TaskReporter::new(slug, state.ui_tx.clone());

    // Build the per-task context (executor connect + scraper prewarm).
    let ctx = TaskContext::build(&cfg, &settings, &private_key)
        .await
        .map_err(|e| e.to_string())?;

    let cancel = CancellationToken::new();
    let hub = Arc::clone(&state.hub);
    let join = tauri::async_runtime::spawn(run_loop(
        cfg,
        deployer,
        selector,
        kind,
        ctx,
        reporter,
        hub,
        cancel.clone(),
    ));
    state.tasks.insert(slug, cancel, join);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    cfg: TaskConfig,
    deployer: Address,
    selector: [u8; 4],
    kind: DecodeKind,
    ctx: TaskContext,
    reporter: TaskReporter,
    hub: Arc<MonitorHub>,
    cancel: CancellationToken,
) {
    // Subscribe first, then register, so no candidate fires between the hub
    // starting its upstream subscription and us having a receiver.
    let mut rx = hub.subscribe_candidates();
    hub.register(deployer).await.ok();

    reporter.stage(TaskStage::Monitoring);
    reporter.info(format!(
        "monitoring deployer {deployer:#x} for selector 0x{} ({kind:?})",
        alloy::primitives::hex::encode(selector)
    ));

    // Known pre-reveal/placeholder URI to ignore (blank → no guard).
    let hidden_uri = cfg.hidden_uri.clone().filter(|u| !u.trim().is_empty());
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            recv = rx.recv() => match recv {
                Ok(cand) => {
                    if cand.from != deployer {
                        continue;
                    }
                    // Every tx from the watched deployer goes to the debug log,
                    // reveal or not, so a missed reveal can be traced afterwards.
                    reporter.debug(format!(
                        "detected tx {:?} from deployer {deployer:#x} ({} bytes calldata)",
                        cand.tx_hash,
                        cand.input.len()
                    ));
                    let base_uri = match decode_reveal_uri(&cand.input, selector, kind) {
                        Ok(u) => u,
                        Err(e) => {
                            reporter.debug(format!("tx {:?} is not a reveal: {e}", cand.tx_hash));
                            continue;
                        }
                    };
                    // A multiConfigure (or any reveal-shaped call) made for a
                    // reason other than revealing re-sends the collection's
                    // still-set hidden baseURI. Firing on it would snipe the
                    // placeholder, so skip the configured pre-reveal URI. The
                    // real reveal sets a different baseURI and passes through.
                    if let Some(hidden) = &hidden_uri {
                        if uris_match(&base_uri, hidden) {
                            reporter.debug(format!(
                                "tx {:?} carries the configured hidden/pre-reveal URI {base_uri} — ignoring",
                                cand.tx_hash
                            ));
                            continue;
                        }
                    }
                    if !seen.insert(base_uri.clone()) {
                        reporter.debug(format!("duplicate reveal {base_uri} ignored"));
                        continue; // already serviced this reveal (mempool + stream dupe)
                    }
                    reporter.debug(format!("reveal URI found: {base_uri}"));
                    let event = RevealEvent { tx_hash: cand.tx_hash, base_uri };
                    if let Err(e) = run_reveal_pipeline(event, &ctx, &reporter).await {
                        tracing::warn!("task {} pipeline error: {e}", cfg.opensea_slug);
                    }
                    // Back to monitoring for any subsequent reveal.
                    reporter.stage(TaskStage::Monitoring);
                }
                Err(RecvError::Lagged(n)) => {
                    reporter.warn(format!("monitor backlog: dropped {n} candidate(s)"));
                }
                Err(RecvError::Closed) => break,
            }
        }
    }

    hub.deregister(deployer).await;
    reporter.stage(TaskStage::Stopped);
    reporter.info("task stopped");
}

/// Compare two base URIs for the hidden-URI guard: trim surrounding whitespace
/// and ignore a single trailing slash (drops vary on whether the baseURI ends
/// in `/`). Case-sensitive, because IPFS CIDs are.
fn uris_match(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}
