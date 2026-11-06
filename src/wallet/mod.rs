//! Wallet store, persisted to `wallet/wallet.json` as a JSON array.
//!
//! Holds the imported buying wallets. Private keys are stored in **plain
//! text**: the app signs with them and the UI can reveal them on request.
//! Treat the `wallet/` directory as a secret and never commit or share it.
//! Balances are cached and only refreshed when the user explicitly asks (no
//! background polling).

use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::store::{atomic_write, config_path, now_unix};

const WALLET_FILE: &str = "wallet/wallet.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletEntry {
    /// Optional human label shown in the UI.
    #[serde(default)]
    pub label: String,
    /// Checksummed `0x…` address derived from `private_key`.
    pub address: String,
    pub private_key: String,
    /// Last fetched balance in ETH. `None` until the user fetches it once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_eth: Option<f64>,
    /// Unix seconds (UTC) of the last balance fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_updated_at: Option<i64>,
}

/// The on-disk wallet list. `#[serde(transparent)]` makes it (de)serialize as a
/// bare JSON array, matching `wallet/wallet.json`'s `[{…}]` shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WalletStore {
    pub wallets: Vec<WalletEntry>,
}

impl WalletStore {
    /// Load from `wallet/wallet.json`, or an empty store if absent/unparseable.
    #[must_use]
    pub fn load() -> Self {
        match std::fs::read_to_string(config_path(WALLET_FILE)) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist atomically.
    ///
    /// # Errors
    /// Returns an error if serialization or the atomic file write fails.
    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.wallets).context("serialize wallets")?;
        atomic_write(&config_path(WALLET_FILE), json.as_bytes())
    }

    /// Import a wallet from a private key, deriving its address. The key is
    /// parsed as a `PrivateKeySigner`, the same way the executor parses it when
    /// a task starts. Rejects a key whose address is already present.
    ///
    /// # Errors
    /// Returns an error if the key is invalid or the wallet already exists.
    pub fn add(&mut self, private_key: &str, label: Option<String>) -> Result<WalletEntry> {
        let key = private_key.trim();
        let signer: PrivateKeySigner = key.parse().context("invalid private key")?;
        let address = signer.address().to_checksum(None);

        if self.find(&address).is_some() {
            anyhow::bail!("wallet {address} is already imported");
        }

        let entry = WalletEntry {
            label: label.unwrap_or_default(),
            address,
            private_key: key.to_string(),
            balance_eth: None,
            balance_updated_at: None,
        };
        self.wallets.push(entry.clone());
        Ok(entry)
    }

    /// Remove a wallet by address (case-insensitive). Returns whether one was
    /// removed.
    pub fn remove(&mut self, address: &str) -> bool {
        let before = self.wallets.len();
        self.wallets
            .retain(|w| !w.address.eq_ignore_ascii_case(address));
        self.wallets.len() != before
    }

    /// Look up a wallet by address (case-insensitive).
    #[must_use]
    pub fn find(&self, address: &str) -> Option<&WalletEntry> {
        self.wallets
            .iter()
            .find(|w| w.address.eq_ignore_ascii_case(address))
    }

    /// Record a freshly fetched balance for `address`, stamping the time.
    pub fn set_balance(&mut self, address: &str, balance_eth: f64) {
        if let Some(w) = self
            .wallets
            .iter_mut()
            .find(|w| w.address.eq_ignore_ascii_case(address))
        {
            w.balance_eth = Some(balance_eth);
            w.balance_updated_at = Some(now_unix());
        }
    }
}

/// Fetch the on-chain ETH balance of `address` via `rpc_url`.
///
/// # Errors
/// Returns an error if the address is invalid, the provider can't connect, or
/// the balance call fails.
pub async fn fetch_balance_eth(rpc_url: &str, address: &str) -> Result<f64> {
    let addr: Address = address.parse().context("invalid address")?;
    let provider = ProviderBuilder::new()
        .connect(rpc_url)
        .await
        .context("RPC connect failed")?;
    let wei = provider
        .get_balance(addr)
        .await
        .context("get_balance failed")?;
    Ok(wei_to_eth(wei))
}

/// Convert wei to ETH as `f64`. ETH balances fit comfortably in `u128`
/// (total supply ≈ 1.2e26 wei ≪ u128::MAX), so the saturating cast is exact in
/// practice. The f64 division is for display only, never for buy math.
fn wei_to_eth(wei: U256) -> f64 {
    let wei128 = u128::try_from(wei).unwrap_or(u128::MAX);
    wei128 as f64 / 1e18
}
