//! Per-task configuration, persisted one file per task at `task/<slug>.json`.
//!
//! A task is the configuration for sniping a single drop: the OpenSea
//! collection slug, validator tip, buy limits, the watched deployer, the buying
//! wallet, rarity targeting, the reveal selector to watch for, and an optional
//! hidden pre-reveal URI to ignore. Multiple tasks can exist and run
//! concurrently against one shared [`MonitorHub`](crate::monitor::MonitorHub).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::mempool::RevealSelector;
use crate::store::{atomic_write, config_path};

const TASK_DIR: &str = "task";

/// The live stage a running task is in, surfaced to the UI as a badge.
/// Serializes lowercase (`"monitoring"`, …) to match the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStage {
    /// Configured but not running.
    Idle,
    /// Running, waiting for a reveal from the monitored deployer.
    Monitoring,
    /// Reveal seen: scraping metadata + fetching listings.
    Scraping,
    /// Targets chosen: submitting buys to relays.
    Buying,
    /// At least one buy confirmed on-chain this cycle.
    Success,
    /// The last reveal cycle errored or produced no confirmed buys.
    Failed,
    /// Stopped by the user.
    Stopped,
}

impl TaskStage {
    /// Lowercase string form used in events and the UI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Monitoring => "monitoring",
            Self::Scraping => "scraping",
            Self::Buying => "buying",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConfig {
    /// OpenSea collection slug, also the task's identity / filename.
    pub opensea_slug: String,
    /// Coinbase bribe paid to the validator, in ETH.
    pub validator_tip_eth: f64,
    /// Max orders per reveal: the rarest buyable targets (within
    /// `top_n_targets` and under `max_price_eth`) are capped at this many and
    /// bundled into one `snipe()`.
    pub max_buys: usize,
    /// Skip any listing priced above this (ETH).
    pub max_price_eth: f64,
    /// NFT contract for the collection (informational, not used by the buy
    /// path).
    pub target_contract: String,
    /// Watched "contract deployer" wallet: the `from` we monitor for a reveal.
    pub deployer: String,
    /// Buying wallet, referenced by address into `wallet/wallet.json`.
    pub wallet: String,
    /// Consider only the rarest N tokens as buy candidates.
    pub top_n_targets: usize,
    /// Proceed once this % of the supply is scraped (1 to 100).
    pub scrape_coverage_pct: u8,
    /// Total token supply, required by the scraper to enumerate token URIs.
    pub collection_supply: u64,
    /// The deployer function call that counts as this drop's reveal, and how to
    /// decode its calldata. Validated when the task starts.
    pub reveal_selector: RevealSelector,
    /// Known pre-reveal / placeholder baseURI for an unrevealed collection.
    /// OpenSea drops keep a hidden baseURI set until reveal, and a
    /// `multiConfigure` call made for any other reason (price, allowlist, …)
    /// re-sends it in the tuple, so the decoder would otherwise fire on the
    /// placeholder. When set, a decoded reveal URI matching this is ignored,
    /// and the real reveal sets a *different* baseURI and passes through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_uri: Option<String>,
}

impl TaskConfig {
    /// Path to this task's JSON file (`task/<slug>.json`).
    #[must_use]
    pub fn path(&self) -> PathBuf {
        task_path(&self.opensea_slug)
    }

    /// Persist atomically to `task/<slug>.json`.
    ///
    /// # Errors
    /// Returns an error if the slug is empty/invalid or the write fails.
    pub fn save(&self) -> Result<()> {
        if sanitize_slug(&self.opensea_slug).is_empty() {
            anyhow::bail!("task slug is empty or invalid");
        }
        let json = serde_json::to_string_pretty(self).context("serialize task")?;
        atomic_write(&self.path(), json.as_bytes())
    }
}

/// Load one task by slug.
///
/// # Errors
/// Returns an error if the file is missing or unparseable.
pub fn load(slug: &str) -> Result<TaskConfig> {
    let path = task_path(slug);
    let s = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))
}

/// List every task in `task/`, skipping any file that fails to parse.
#[must_use]
pub fn list() -> Vec<TaskConfig> {
    let Ok(entries) = std::fs::read_dir(config_path(TASK_DIR)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<TaskConfig>(&s) {
                out.push(cfg);
            }
        }
    }
    out.sort_by(|a, b| a.opensea_slug.cmp(&b.opensea_slug));
    out
}

/// Delete a task's file. Returns whether a file was removed.
///
/// # Errors
/// Returns an error only if removal fails for a reason other than "not found".
pub fn delete(slug: &str) -> Result<bool> {
    let path = task_path(slug);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    }
}

fn task_path(slug: &str) -> PathBuf {
    config_path(TASK_DIR).join(format!("{}.json", sanitize_slug(slug)))
}

/// Restrict a slug to filename-safe characters so it can't escape `task/`.
fn sanitize_slug(slug: &str) -> String {
    slug.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}
