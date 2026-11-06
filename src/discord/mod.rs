//! Discord webhook notifications.
//!
//! Two messages: a [`send_test`] embed for the Settings "test" button, and a
//! [`send_snipe_result`] embed fired **after** a reveal cycle finishes, never
//! on the hot path. The pipeline collects confirmed buys during the cycle and
//! spawns the sends once it is done, so webhook latency can't delay a bundle
//! broadcast.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use std::time::Duration;

/// Embed accent (the colored left bar): Shard's blue-slate primary `#6C7893`,
/// matching the UI's `primary` color.
const EMBED_COLOR: u32 = 0x6C_78_93;
const APP_NAME: &str = "Shard";
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Result of one buy attempt, rendered into a Discord embed.
#[derive(Debug, Clone, Serialize)]
pub struct SnipeReport {
    pub collection: String,
    pub token_id: u64,
    pub tx_hash: String,
    pub price_eth: f64,
    pub tip_eth: f64,
    pub rank: u64,
    pub rarity_score: f64,
    pub wallet: String,
    pub block: Option<u64>,
}

/// Post the Settings test embed: a title, a short message, a "functional"
/// status field, and a timestamp.
///
/// # Errors
/// Returns an error if the URL is empty, the request fails, or Discord returns
/// a non-2xx status.
pub async fn send_test(webhook_url: &str) -> Result<()> {
    let now = Utc::now();
    let body = json!({
        "username": APP_NAME,
        "embeds": [{
            "title": "Test Webhook",
            "description": "Test webhook message.",
            "color": EMBED_COLOR,
            "fields": [
                { "name": "Status", "value": "Webhook is functional! \u{1F525}", "inline": true },
                { "name": "Timestamp", "value": now.format("%Y-%m-%d %I:%M:%S %p").to_string(), "inline": true }
            ],
            "footer": { "text": APP_NAME },
            "timestamp": now.to_rfc3339(),
        }]
    });
    post(webhook_url, &body).await
}

/// Post a snipe-result embed: links the tx, names the token, shows price + tip,
/// rarity, and the buying wallet. Fired post-cycle, off the hot path.
///
/// # Errors
/// Returns an error if the URL is empty, the request fails, or Discord returns
/// a non-2xx status.
pub async fn send_snipe_result(webhook_url: &str, report: &SnipeReport) -> Result<()> {
    let now = Utc::now();
    let tx_url = format!("https://etherscan.io/tx/{}", report.tx_hash);
    // Only confirmed fills are reported, so the status is always "confirmed".
    let title = format!(
        "\u{1F3AF} Sniped {} #{}",
        report.collection, report.token_id
    );
    let status = "Confirmed on-chain";

    let block = report
        .block
        .map_or_else(|| "—".to_string(), |b| b.to_string());

    let body = json!({
        "username": APP_NAME,
        "embeds": [{
            "title": title,
            "url": tx_url,
            "color": EMBED_COLOR,
            "fields": [
                { "name": "Token", "value": format!("#{}", report.token_id), "inline": true },
                { "name": "Rarity", "value": format!("Rank {} \u{00B7} score {:.2}", report.rank, report.rarity_score), "inline": true },
                { "name": "Status", "value": status, "inline": true },
                { "name": "Price", "value": format!("{:.4} \u{039E}", report.price_eth), "inline": true },
                { "name": "Validator tip", "value": format!("{:.4} \u{039E}", report.tip_eth), "inline": true },
                { "name": "Total", "value": format!("{:.4} \u{039E}", report.price_eth + report.tip_eth), "inline": true },
                { "name": "Wallet", "value": format!("`{}`", report.wallet), "inline": false },
                { "name": "Block", "value": block, "inline": true },
                { "name": "Transaction", "value": format!("[View on Etherscan]({tx_url})"), "inline": false }
            ],
            "footer": { "text": APP_NAME },
            "timestamp": now.to_rfc3339(),
        }]
    });
    post(webhook_url, &body).await
}

async fn post(webhook_url: &str, body: &serde_json::Value) -> Result<()> {
    let url = webhook_url.trim();
    if url.is_empty() {
        anyhow::bail!("discord webhook url is empty");
    }
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
        .context("build discord http client")?;
    let resp = client
        .post(url)
        .json(body)
        .send()
        .await
        .context("discord webhook request failed")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("discord webhook returned {status}: {text}");
    }
    Ok(())
}
