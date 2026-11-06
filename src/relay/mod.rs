//! Bundle broadcast to block-builder relays.
//!
//! A signed snipe tx is submitted via `eth_sendBundle` to every configured
//! relay (e.g. Titan, Quasar, BuilderNet) instead of the public mempool. Each
//! tx is sent in two block variants per relay:
//!   * a *blank* `blockNumber` (Titan/Quasar default to the current block,
//!     BuilderNet to the next), covering the mempool case (land in block N), and
//!   * an explicit `head + 1`, covering the mined-block case, where the reveal
//!     is already in block N (e.g. seen via QuickNode Streams, so land in N+1).
//!
//! Authentication is the optional-but-prioritized `X-Flashbots-Signature`
//! header: an EIP-191 signature over `keccak256(body)` rendered as a hex string,
//! matching the canonical ethers `wallet.signMessage(ethers.id(JSON))` flow.
//! The buying wallet's key signs it. A consistent signing key builds relay
//! reputation.

use alloy::primitives::keccak256;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use reqwest::Client;
use serde::Serialize;
use tracing::{debug, warn};

/// Tally of one [`RelayClient::broadcast`] fan-out: how many relay POSTs
/// returned HTTP 2xx (`accepted`) out of how many were sent (`attempted`).
/// `accepted == 0` means the bundle never entered any builder's pool: a
/// stage-1 (inclusion) failure, distinct from "accepted but lost the block
/// auction," which still reports `accepted > 0`.
#[derive(Clone, Copy, Debug)]
pub struct BroadcastStats {
    pub accepted: usize,
    pub attempted: usize,
}

#[derive(Serialize)]
struct BundleParams<'a> {
    txs: [&'a str; 1],
    #[serde(rename = "blockNumber", skip_serializing_if = "Option::is_none")]
    block_number: Option<String>,
}

#[derive(Serialize)]
struct BundleRequest<'a> {
    jsonrpc: &'static str,
    id: u32,
    method: &'static str,
    params: [BundleParams<'a>; 1],
}

/// Broadcasts signed transactions to a fixed set of relays.
///
/// Owned by `SeaportExecutor`, whose `reqwest::Client` it shares, so relay and
/// OpenSea connections stay warm in one pool.
pub struct RelayClient {
    http: Client,
    signer: PrivateKeySigner,
    relays: Vec<String>,
}

impl RelayClient {
    pub fn new(http: Client, signer: PrivateKeySigner, relays: Vec<String>) -> Self {
        Self {
            http,
            signer,
            relays,
        }
    }

    /// Fan out the raw signed tx to every relay, once with a blank target block
    /// and once explicitly targeting `head + 1`. Relay failures are logged and
    /// never abort the others, because partial delivery still wins blocks.
    pub async fn broadcast(&self, raw_tx_hex: &str, head: u64) -> BroadcastStats {
        // Sign each body once (the payload is identical across relays). Only the
        // block target distinguishes the two variants.
        let variants = [
            self.build_signed(raw_tx_hex, None).await,
            self.build_signed(raw_tx_hex, Some(head + 1)).await,
        ];

        let mut sends = Vec::with_capacity(self.relays.len() * variants.len());
        for variant in &variants {
            match variant {
                Ok((body, header)) => {
                    for relay in &self.relays {
                        sends.push(self.post(relay, body, header));
                    }
                }
                Err(e) => warn!("bundle signing failed, skipping variant: {}", e),
            }
        }
        let outcomes = futures::future::join_all(sends).await;
        BroadcastStats {
            accepted: outcomes.iter().filter(|&&ok| ok).count(),
            attempted: outcomes.len(),
        }
    }

    /// Build the JSON body and its `address:0x<sig>` auth header.
    async fn build_signed(
        &self,
        raw_tx_hex: &str,
        block: Option<u64>,
    ) -> anyhow::Result<(String, String)> {
        let req = BundleRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "eth_sendBundle",
            params: [BundleParams {
                txs: [raw_tx_hex],
                block_number: block.map(|b| format!("0x{:x}", b)),
            }],
        };
        let body = serde_json::to_string(&req)?;

        // EIP-191 over the keccak256 of the body, rendered as a 0x hex string,
        // identical to ethers `signMessage(ethers.id(JSON.stringify(body)))`.
        let hash_hex = format!("0x{:x}", keccak256(body.as_bytes()));
        let sig = self.signer.sign_message(hash_hex.as_bytes()).await?;
        let header = format!(
            "{}:0x{}",
            self.signer.address().to_checksum(None),
            hex::encode(sig.as_bytes())
        );
        Ok((body, header))
    }

    /// Returns `true` if the relay accepted the bundle (HTTP 2xx).
    async fn post(&self, relay: &str, body: &str, header: &str) -> bool {
        match self
            .http
            .post(relay)
            .header("Content-Type", "application/json")
            .header("X-Flashbots-Signature", header)
            .body(body.to_owned())
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    debug!("relay {} accepted bundle ({})", relay, status);
                    true
                } else {
                    let text = resp.text().await.unwrap_or_default();
                    warn!("relay {} rejected bundle {}: {}", relay, status, text);
                    false
                }
            }
            Err(e) => {
                warn!("relay {} send failed: {}", relay, e);
                false
            }
        }
    }
}
