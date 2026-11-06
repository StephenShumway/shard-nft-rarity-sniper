//! Global settings, persisted to `settings/settings.json`.
//!
//! Everything the bot needs that is *not* per-task and *not* a wallet lives
//! here: RPC, bloxroute, IPFS gateways, relays, OpenSea key, IPFS concurrency,
//! the QuickNode webhook config, and the Discord webhook. The deployed
//! `sniper_contract` is persisted and hand-editable in the JSON but is
//! intentionally **never** surfaced to (or settable from) the desktop UI (see
//! [`Settings::for_ui`] and [`Settings::merge_from_ui`]).

use alloy::primitives::Address;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::ipfs::Gateway;
use crate::store::{atomic_write, config_path};

const SETTINGS_FILE: &str = "settings/settings.json";

/// Placeholder for the deployed RaritySniper contract. It is only used when
/// `settings/settings.json` is first created or lacks the field. After that,
/// set the real address by editing `sniper_contract` in that file (it is never
/// exposed in the frontend). Must be replaced before running live.
const DEFAULT_SNIPER_CONTRACT: &str = "0x0000000000000000000000000000000000000000";

const DEFAULT_IPFS_CONCURRENCY: usize = 50;
const DEFAULT_STREAM_PORT: u16 = 3000;

/// Global, non-wallet configuration. `#[serde(default)]` means a partial or
/// older `settings.json` still loads, because missing fields fall back to
/// [`Settings::default`] rather than failing the whole parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Ethereum JSON-RPC endpoint (HTTPS). Used for nonce/gas/fees, balances,
    /// and the RPC latency test.
    pub eth_rpc_url: String,
    /// bloxroute pending-tx WebSocket endpoint.
    pub blox_stream_url: String,
    /// bloxroute `Authorization` header value.
    pub blox_auth_header: String,
    /// Pinata / IPFS gateways the scraper round-robins across.
    pub gateways: Vec<Gateway>,
    pub opensea_api_key: String,
    /// Max in-flight IPFS requests allowed *per gateway*.
    pub ipfs_concurrency: usize,
    /// Block-builder relay endpoints (`eth_sendBundle` targets).
    pub relays: Vec<String>,
    /// Local TCP port the QuickNode webhook receiver binds.
    pub stream_port: u16,
    /// QuickNode Stream security token (HMAC key). `None`/empty disables auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qn_security_token: Option<String>,
    /// Discord webhook URL for snipe-result notifications. `None`/empty disables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord_webhook: Option<String>,
    /// Deployed RaritySniper contract, persisted but hidden from the UI.
    pub sniper_contract: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            eth_rpc_url: String::new(),
            blox_stream_url: String::new(),
            blox_auth_header: String::new(),
            gateways: Vec::new(),
            opensea_api_key: String::new(),
            ipfs_concurrency: DEFAULT_IPFS_CONCURRENCY,
            relays: Vec::new(),
            stream_port: DEFAULT_STREAM_PORT,
            qn_security_token: None,
            discord_webhook: None,
            sniper_contract: DEFAULT_SNIPER_CONTRACT.to_string(),
        }
    }
}

impl Settings {
    /// Load from `settings/settings.json`, falling back to [`Settings::default`]
    /// if the file is absent or unparseable (first run / corrupt write).
    #[must_use]
    pub fn load() -> Self {
        match std::fs::read_to_string(config_path(SETTINGS_FILE)) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist atomically to `settings/settings.json`.
    ///
    /// # Errors
    /// Returns an error if serialization or the atomic file write fails.
    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(self).context("serialize settings")?;
        atomic_write(&config_path(SETTINGS_FILE), json.as_bytes())
    }

    /// A copy safe to hand to the frontend: the hidden `sniper_contract` is
    /// blanked so it can neither be read nor round-tripped back through
    /// `save_settings`. The persisted value on disk is untouched.
    #[must_use]
    pub fn for_ui(&self) -> Self {
        let mut s = self.clone();
        s.sniper_contract = String::new();
        s
    }

    /// Merge a UI-submitted settings payload into `self`. The hidden
    /// `sniper_contract` is *always* taken from the stored value: the UI only
    /// ever receives it blanked (see [`Settings::for_ui`]), so accepting the
    /// incoming value would wipe the contract on every save, and the UI must not
    /// be able to set it anyway. Returns the merged value so the caller can
    /// persist it and swap it into shared state.
    #[must_use]
    pub fn merge_from_ui(&self, mut incoming: Self) -> Self {
        incoming.sniper_contract = self.sniper_contract.clone();
        incoming
    }

    /// Parse the configured sniper contract into an [`Address`].
    ///
    /// # Errors
    /// Returns an error if the stored value is not a valid address.
    pub fn sniper_contract_addr(&self) -> Result<Address> {
        self.sniper_contract
            .parse()
            .with_context(|| format!("invalid sniper_contract address {:?}", self.sniper_contract))
    }
}
