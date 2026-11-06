//! The reveal → buy pipeline, run per task by the desktop app. A
//! [`TaskContext`] bundles the components built once when a task starts
//! (scraper, OpenSea client, executor) with the per-task buy parameters.
//! [`run_reveal_pipeline`] runs one reveal cycle and reports its stage
//! transitions through a [`TaskReporter`].

use alloy::primitives::U256;
use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::discord::{self, SnipeReport};
use crate::ipfs::IpfsScraper;
use crate::mempool::RevealEvent;
use crate::opensea::{Listing, OpenSeaClient};
use crate::rarity::RarityEngine;
use crate::reporter::TaskReporter;
use crate::seaport::{BatchOrder, OrderResult, SeaportExecutor};
use crate::settings::Settings;
use crate::task::{TaskConfig, TaskStage};

/// Everything one running task needs to service a reveal. Built once on task
/// start (an executor connect + scraper prewarm), then reused for every reveal
/// the task sees.
pub struct TaskContext {
    pub slug: String,
    pub collection_supply: u64,
    pub top_n_targets: usize,
    pub max_price_eth: f64,
    pub max_buys: usize,
    /// Validator tip in ETH, for the Discord report (the executor already holds
    /// the wei value it spends).
    pub validator_tip_eth: f64,
    /// Buying wallet address, shown in the Discord report.
    pub wallet: String,
    /// Discord webhook URL, if configured. Sent to after a cycle, off hot path.
    pub discord_webhook: Option<String>,
    pub scraper: Arc<IpfsScraper>,
    pub opensea: Arc<OpenSeaClient>,
    pub executor: Arc<SeaportExecutor>,
}

impl TaskContext {
    /// Build the per-task context: OpenSea client + IPFS scraper (prewarmed) +
    /// Seaport executor, wired from global [`Settings`] and the task's own
    /// config + buying key.
    ///
    /// # Errors
    /// Returns an error if the sniper contract address is invalid or the
    /// executor fails to build (bad private key, RPC connect, chain-id fetch).
    pub async fn build(cfg: &TaskConfig, settings: &Settings, private_key: &str) -> Result<Self> {
        let opensea = Arc::new(OpenSeaClient::new(
            settings.opensea_api_key.clone(),
            cfg.opensea_slug.clone(),
        ));

        let scraper = Arc::new(IpfsScraper::new(
            settings.gateways.clone(),
            settings.ipfs_concurrency,
            cfg.scrape_coverage_pct,
        ));

        let sniper_contract = settings.sniper_contract_addr()?;
        let validator_tip_wei = U256::from((cfg.validator_tip_eth * 1e18) as u128);

        // Prewarm the IPFS pools and connect the executor concurrently. Both
        // must be ready before the first reveal can be serviced.
        let prewarm_fut = scraper.prewarm(20);
        let executor_fut = SeaportExecutor::new(
            &settings.eth_rpc_url,
            private_key,
            settings.opensea_api_key.clone(),
            settings.relays.clone(),
            sniper_contract,
            validator_tip_wei,
        );
        let (_, executor_result) = tokio::join!(prewarm_fut, executor_fut);
        let executor = Arc::new(executor_result.context("executor connect failed")?);

        Ok(Self {
            slug: cfg.opensea_slug.clone(),
            collection_supply: cfg.collection_supply,
            top_n_targets: cfg.top_n_targets,
            max_price_eth: cfg.max_price_eth,
            max_buys: cfg.max_buys,
            validator_tip_eth: cfg.validator_tip_eth,
            wallet: cfg.wallet.clone(),
            discord_webhook: settings
                .discord_webhook
                .clone()
                .filter(|u| !u.trim().is_empty()),
            scraper,
            opensea,
            executor,
        })
    }
}

/// Run one reveal cycle: scrape + listings → rarity score → buy the buyable
/// top-N via the snipe contract → relays. Emits `Scraping` / `Buying` /
/// `Success` / `Failed` stage transitions through `reporter`. Errors are
/// returned *and* reported as `Failed`, so the caller can simply log them.
///
/// # Errors
/// Returns an error if the scrape yields nothing, listings fail, rarity scoring
/// panics, or the pre-buy nonce/head fetch fails.
pub async fn run_reveal_pipeline(
    event: RevealEvent,
    ctx: &TaskContext,
    reporter: &TaskReporter,
) -> Result<()> {
    match run_inner(event, ctx, reporter).await {
        Ok(buys_made) => {
            if buys_made > 0 {
                reporter.stage(TaskStage::Success);
            } else {
                reporter.stage(TaskStage::Failed);
            }
            Ok(())
        }
        Err(e) => {
            reporter.error(format!("pipeline error: {e}"));
            reporter.stage(TaskStage::Failed);
            Err(e)
        }
    }
}

/// Body of [`run_reveal_pipeline`]. Returns the number of confirmed buys.
async fn run_inner(
    event: RevealEvent,
    ctx: &TaskContext,
    reporter: &TaskReporter,
) -> Result<usize> {
    let start = std::time::Instant::now();
    reporter.stage(TaskStage::Scraping);
    reporter.info(format!("reveal detected → {}", event.base_uri));
    reporter.debug(format!(
        "reveal tx {:?} base_uri={} supply={}",
        event.tx_hash, event.base_uri, ctx.collection_supply
    ));

    // Parallel scrape + listings (both external-I/O bound). The scrape reports
    // a coverage percentage to the UI as it goes, and `reporter.progress` is
    // itself throttled, so this never floods the IPC bridge.
    let progress_reporter = reporter.clone();
    let scrape_fut = ctx.scraper.scrape_collection_with_progress(
        &event.base_uri,
        ctx.collection_supply,
        move |done, total| progress_reporter.progress(done, total),
    );
    let listings_fut = ctx.opensea.fetch_all_listings();
    let (tokens, listings_result) = tokio::join!(scrape_fut, listings_fut);
    let listings = listings_result.context("listings fetch failed mid-pipeline")?;

    if tokens.is_empty() {
        anyhow::bail!("scrape returned 0 tokens");
    }
    reporter.info(format!(
        "{} tokens scraped, {} listings ({:.2}s)",
        tokens.len(),
        listings.len(),
        start.elapsed().as_secs_f32()
    ));
    reporter.debug(format!(
        "scrape complete: {} tokens, {} unique listings in {:.2}s",
        tokens.len(),
        listings.len(),
        start.elapsed().as_secs_f32()
    ));

    // CPU-bound rarity scoring on a blocking thread.
    let scored = tokio::task::spawn_blocking(move || RarityEngine::score(&tokens))
        .await
        .context("rarity scoring task panicked")?;

    // Persist the rank-ordered top-N token ids for downstream tooling. Spawned
    // so the write never blocks the buys.
    {
        let csv: String = scored
            .iter()
            .take(ctx.top_n_targets)
            .map(|t| t.token_id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let slug = ctx.slug.clone();
        tokio::spawn(async move {
            if let Err(e) = write_rankings(&slug, &csv).await {
                warn!("rankings file write failed: {e}");
            }
        });
    }

    // Compare in integer wei against the listing's exact `price_wei` rather
    // than in f64 ETH, so a listing at the boundary isn't lost to float
    // rounding. Carry the rarity score through so the Discord report can show it.
    let max_price_wei: u128 = (ctx.max_price_eth * 1e18) as u128;
    let targets: Vec<(u64, u64, f64, Listing)> = scored
        .iter()
        .take(ctx.top_n_targets)
        .filter_map(|token| {
            listings.get(&token.token_id).and_then(|listing| {
                (listing.price_wei <= max_price_wei).then(|| {
                    (
                        token.token_id,
                        token.rank,
                        token.rarity_score,
                        listing.clone(),
                    )
                })
            })
        })
        .take(ctx.max_buys)
        .collect();

    let attempts = targets.len();
    reporter.stage(TaskStage::Buying);
    reporter.info(format!(
        "{attempts} buyable target(s) in top {} (max_buys {})",
        ctx.top_n_targets, ctx.max_buys
    ));
    for (token_id, rank, score, listing) in &targets {
        reporter.debug(format!(
            "target token #{token_id} rank {rank} score {score:.2} price {:.4} ETH order {}",
            listing.price_eth, listing.order_hash
        ));
    }

    if attempts == 0 {
        return Ok(0);
    }

    // Fetch confirmed nonce + head once, late, so both are fresh. All targets go
    // in ONE snipe() at this nonce, and head + 1 is the explicit bundle target
    // block.
    let (base_nonce, head) = ctx
        .executor
        .fetch_nonce_and_head()
        .await
        .context("failed to fetch nonce/head before buys")?;
    reporter.debug(format!("nonce {base_nonce}, target block {}", head + 1));

    // Keep reporting metadata index-aligned with the orders so the single
    // snipe()'s per-order result maps back to token/rank/score/price.
    let meta: Vec<(u64, u64, f64, f64)> = targets
        .iter()
        .map(|(token_id, rank, score, listing)| (*token_id, *rank, *score, listing.price_eth))
        .collect();
    let orders: Vec<BatchOrder> = targets
        .into_iter()
        .map(|(_, rank, _, listing)| BatchOrder { listing, rank })
        .collect();

    reporter.debug(format!(
        "sending batch snipe: {} order(s), nonce {base_nonce}, block {}",
        orders.len(),
        head + 1
    ));

    // One batched snipe(). A send/confirm failure ends the cycle gracefully
    // (0 buys) rather than erroring out the whole pipeline.
    let batch = match ctx.executor.buy_batch(&orders, base_nonce, head).await {
        Ok(b) => b,
        Err(e) => {
            reporter.warn(format!("snipe send/confirm failed: {e}"));
            reporter.info(format!(
                "cycle complete: 0 confirmed of {attempts} ({:.2}s)",
                start.elapsed().as_secs_f32()
            ));
            return Ok(0);
        }
    };

    let tx = batch.tx_hash.map(|h| format!("{h:?}"));

    // Inclusion-stage diagnostics (did a builder include us?) are reported
    // separately from the per-order fill results (stage 2) below.
    reporter.info(format!(
        "relays accepted {}/{} • targeted block {} (head+1)",
        batch.relay_accepted, batch.relay_attempted, batch.target_block
    ));
    match batch.block_number {
        Some(landed) => {
            let delta = landed.saturating_sub(head);
            reporter.info(format!(
                "bundle landed in block {landed} (head+{delta}) — the blank-block variant floats past head+1 when send latency crosses a slot"
            ));
        }
        None if batch.tx_hash.is_some() => {
            // Sent but no receipt: distinguish "never accepted" from "accepted
            // but not included" so a miss points at the right knob to turn.
            if batch.relay_accepted == 0 {
                reporter.warn(format!(
                    "bundle never sent — 0/{} relays accepted it (inclusion failure: check relay URLs / signing auth)",
                    batch.relay_attempted
                ));
            } else {
                reporter.warn(format!(
                    "bundle accepted by {}/{} relays but never included in block {} — lost the auction (raise the tip or widen the relay set)",
                    batch.relay_accepted, batch.relay_attempted, batch.target_block
                ));
            }
        }
        None => {} // every order failed prep, so nothing sent (see `Unprepared` below)
    }

    if batch.tx_reverted {
        reporter.warn(format!(
            "snipe tx reverted — no orders filled (tx {})",
            tx.as_deref().unwrap_or("-")
        ));
    }

    // Map each target's on-chain disposition back to a report line. Confirmed
    // buys are collected for the post-cycle Discord notify (off hot path).
    let mut buys_made = 0usize;
    let mut reports: Vec<SnipeReport> = Vec::new();
    for (i, (token_id, rank, score, price_eth)) in meta.into_iter().enumerate() {
        match batch.results[i] {
            OrderResult::Filled => {
                let tx = tx.clone().unwrap_or_default();
                reporter.info(format!("rank {rank} confirmed (tx {tx})"));
                reporter.debug(format!(
                    "buy confirmed: token #{token_id} rank {rank} tx {tx} block {:?}",
                    batch.block_number
                ));
                buys_made += 1;
                reports.push(SnipeReport {
                    collection: ctx.slug.clone(),
                    token_id,
                    tx_hash: tx.clone(),
                    price_eth,
                    tip_eth: ctx.validator_tip_eth,
                    rank,
                    rarity_score: score,
                    wallet: ctx.wallet.clone(),
                    block: batch.block_number,
                });
            }
            OrderResult::NotFilled => reporter.debug(format!(
                "not filled: token #{token_id} rank {rank} (skipped on-chain or tx reverted)"
            )),
            OrderResult::Unprepared => reporter.debug(format!(
                "unprepared: token #{token_id} rank {rank} (OpenSea fulfillment failed — not sent)"
            )),
        }
    }

    reporter.info(format!(
        "cycle complete: {buys_made} confirmed of {attempts} ({:.2}s)",
        start.elapsed().as_secs_f32()
    ));
    debug!("task {} reveal cycle: {buys_made}/{attempts}", ctx.slug);

    // Fire Discord notifications for confirmed buys, spawned so the (network)
    // send never blocks return, and only after the cycle so it can't touch the
    // hot path.
    if let Some(webhook) = &ctx.discord_webhook {
        for report in reports {
            let url = webhook.clone();
            let reporter = reporter.clone();
            tokio::spawn(async move {
                if let Err(e) = discord::send_snipe_result(&url, &report).await {
                    reporter.debug(format!("discord webhook failed: {e}"));
                }
            });
        }
    }

    Ok(buys_made)
}

async fn write_rankings(slug: &str, csv: &str) -> Result<()> {
    tokio::fs::create_dir_all("rankings").await?;
    tokio::fs::write(format!("rankings/{slug}.txt"), csv).await?;
    Ok(())
}
