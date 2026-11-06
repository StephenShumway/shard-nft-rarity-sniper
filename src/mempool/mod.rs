//! Reveal-calldata decoding and bloxroute message helpers.
//!
//! Holds the per-task [`RevealSelector`], the base-URI decoder
//! ([`decode_reveal_uri`]) and the candidate/reveal types passed from the
//! [`MonitorHub`](crate::monitor::MonitorHub) to each task. There is no
//! built-in selector list: every task names the exact function call that
//! counts as its reveal. The bloxroute WebSocket connection itself is owned by
//! the hub, and this module only builds and parses its messages.

use alloy::primitives::{Address, Bytes, B256};
use alloy::sol;
use alloy::sol_types::SolCall;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Classifies how a reveal selector's calldata should be decoded. Serialized as
/// `"Typed"` / `"MultiConfigure"` in a task's `reveal_selector`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DecodeKind {
    /// Calldata is `function(string baseURI)`: typed ABI decode.
    Typed,
    /// Calldata is a multiConfigure-style tuple: raw byte scan for the URI.
    MultiConfigure,
}

/// A task's reveal selector: which function call from the watched deployer
/// counts as the reveal, and how to pull the base URI out of its calldata.
/// Required to start a task and stored as `reveal_selector` in `task/<slug>.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevealSelector {
    /// 4-byte selector as hex, e.g. `"0x55f804b3"` for `setBaseURI(string)`.
    pub selector: String,
    pub kind: DecodeKind,
}

impl RevealSelector {
    /// Parse the configured hex selector into its 4 bytes.
    ///
    /// # Errors
    /// Returns an error if the value isn't exactly 4 bytes of hex.
    pub fn selector_bytes(&self) -> Result<[u8; 4]> {
        let s = self.selector.trim().trim_start_matches("0x");
        let bytes = hex::decode(s).context("reveal selector is not valid hex")?;
        <[u8; 4]>::try_from(bytes.as_slice())
            .map_err(|_| anyhow::anyhow!("reveal selector must be exactly 4 bytes"))
    }
}

sol! {
    function setBaseURI(string baseURI);
}

/// A decoded reveal. The downstream pipeline only needs the base URI (plus the
/// tx hash for logging). Each task builds one after decoding a [`CandidateTx`].
#[derive(Debug, Clone)]
pub struct RevealEvent {
    pub tx_hash: B256,
    pub base_uri: String,
}

/// A raw reveal *candidate*: a tx from a monitored deployer, pre-decode. The
/// [`MonitorHub`](crate::monitor::MonitorHub) broadcasts these, and each task
/// filters by `from` and decodes with its own reveal selector. This is what
/// lets every task use a different selector over one shared stream.
#[derive(Debug, Clone)]
pub struct CandidateTx {
    pub from: Address,
    pub tx_hash: B256,
    pub input: Bytes,
}

/// Build a [`CandidateTx`] from raw string parts (hex tx hash / from / input).
/// Shared by the bloxroute and QuickNode paths. Returns `None` if `from` or the
/// input hex don't parse.
pub(crate) fn candidate_from_parts(
    from: &str,
    tx_hash: &str,
    input_hex: &str,
) -> Option<CandidateTx> {
    let from: Address = from.parse().ok()?;
    let tx_hash: B256 = tx_hash.parse().unwrap_or(B256::ZERO);
    let input = decode_hex(input_hex).ok()?;
    Some(CandidateTx {
        from,
        tx_hash,
        input: Bytes::from(input),
    })
}

/// Builds the bloxroute `subscribe` message for one deployer address with the
/// given JSON-RPC request `id` (the hub uses distinct ids to correlate acks).
///
/// Lowercase `from` is REQUIRED because bloxroute's pendingTxs filter index stores
/// `from` lowercase and the comparison is case-sensitive. We also request
/// `tx_contents.from` so consumers can route by sender: every deployer's
/// subscription shares one connection, so each notification has to say who
/// sent it.
///
/// Note: the `include` keys are snake_case (`tx_hash`, `tx_contents.input`,
/// `tx_contents.from`) but the JSON response is camelCase (`txHash`,
/// `txContents`). Don't "fix" the include list to camelCase. It'll be rejected.
pub(crate) fn build_subscribe_message(target: &Address, id: u64) -> String {
    let filter = format!("({{from}} IN ['{target:#x}'])");
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "subscribe",
        "params": [
            "pendingTxs",
            {
                "include": ["tx_hash", "tx_contents.input", "tx_contents.from"],
                "filters": filter,
            }
        ]
    })
    .to_string()
}

/// Builds a bloxroute `unsubscribe` message for a previously returned
/// subscription id.
pub(crate) fn build_unsubscribe_message(subscription_id: &str, id: u64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "unsubscribe",
        "params": [subscription_id],
    })
    .to_string()
}

/// Builds a tungstenite client request with the bloxroute Authorization header.
pub(crate) fn build_ws_request(
    url: &str,
    auth_header: &str,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let mut request = url
        .into_client_request()
        .with_context(|| format!("invalid WS url: {url}"))?;
    let value = auth_header
        .parse()
        .context("bloxroute Authorization header is not a valid HTTP header value")?;
    request.headers_mut().insert("Authorization", value);
    Ok(request)
}

/// Extracts `(from, tx_hash, input_hex)` from a bloxroute pending-tx
/// notification. Returns `None` for any other message shape (subscribe ack,
/// unsubscribe ack, etc).
pub(crate) fn parse_pending_tx_notification(text: &str) -> Option<(String, String, String)> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let result = json.get("params")?.get("result")?;
    let tx_hash = result.get("txHash")?.as_str()?.to_string();
    let contents = result.get("txContents")?;
    let input = contents.get("input")?.as_str()?.to_string();
    let from = contents.get("from")?.as_str()?.to_string();
    Some((from, tx_hash, input))
}

/// Parses a bloxroute `subscribe` ack `{"id": <req_id>, "result": "<sub_id>"}`
/// into `(req_id, subscription_id)`. Returns `None` for non-ack messages.
pub(crate) fn parse_subscription_ack(text: &str) -> Option<(u64, String)> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    // A notification has `params`, but an ack has a top-level string `result`.
    if json.get("params").is_some() {
        return None;
    }
    let id = json.get("id")?.as_u64()?;
    let sub = json.get("result")?.as_str()?.to_string();
    Some((id, sub))
}

pub(crate) fn decode_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim_start_matches("0x");
    Ok(hex::decode(s)?)
}

/// Decode the new base URI from a candidate tx's calldata, if the tx calls the
/// task's reveal `selector`. This is the entry point each task uses. The
/// selector is parsed and validated once, when the task starts.
///
/// # Errors
/// Returns an error if the calldata doesn't start with `selector`, or if its
/// body doesn't decode as `kind`.
pub fn decode_reveal_uri(input: &Bytes, selector: [u8; 4], kind: DecodeKind) -> Result<String> {
    if input.len() < 4 || input[..4] != selector {
        anyhow::bail!(
            "selector 0x{} is not this task's reveal selector",
            hex::encode(&input[..input.len().min(4)])
        );
    }
    match kind {
        DecodeKind::Typed => decode_typed_string_arg(input),
        DecodeKind::MultiConfigure => decode_multi_configure_base_uri(input),
    }
}

/// Decode `<4-byte selector> ++ abi.encode(string)`. The ABI body is identical
/// for any single-`string` function, whatever its name, so swap in
/// `setBaseURI`'s selector and let alloy's typed decoder (which rejects a
/// mismatched selector) decode the body. Caller guarantees `input.len() >= 4`.
fn decode_typed_string_arg(input: &Bytes) -> Result<String> {
    let mut buf = input.to_vec();
    buf[0..4].copy_from_slice(&setBaseURICall::SELECTOR);
    let call = setBaseURICall::abi_decode(&buf)
        .map_err(|e| anyhow::anyhow!("typed reveal decode failed: {e}"))?;
    // Strip stray surrounding whitespace. A deployer fat-fingering a trailing
    // space into setBaseURI (seen on-chain) otherwise yields malformed token
    // URLs (`…re/ /1`, since `trim_end_matches('/')` can't see past the space)
    // and a 0-token scrape. multiConfigure already trims, so keep both paths uniform.
    Ok(call.baseURI.trim().to_string())
}

fn decode_multi_configure_base_uri(input: &Bytes) -> Result<String> {
    // multiConfigure takes a large struct. Rather than dyn_abi-decode all of
    // it, scan the raw bytes for a known URI prefix and cut at the first NUL
    // (the ABI zero-padding after the string). Cheap but fragile: the first
    // `ipfs://` anywhere in the calldata beats any `https://`, so another URI
    // field in the struct (e.g. contractURI) can be picked up instead.
    let data = &input[4..];
    let raw_str = String::from_utf8_lossy(data);

    for prefix in ["ipfs://", "https://"] {
        if let Some(pos) = raw_str.find(prefix) {
            let tail = &raw_str[pos..];
            let end = tail.find('\0').unwrap_or(tail.len());
            let uri = tail[..end].trim();
            if !uri.is_empty() {
                return Ok(uri.to_string());
            }
        }
    }

    anyhow::bail!("Could not find baseURI in multiConfigure calldata")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<selector> ++ abi.encode(uri)`: the calldata of any `fn(string)`.
    fn string_call(selector: [u8; 4], uri: &str) -> Bytes {
        let mut v = setBaseURICall {
            baseURI: uri.into(),
        }
        .abi_encode();
        v[..4].copy_from_slice(&selector);
        Bytes::from(v)
    }

    #[test]
    fn typed_decodes_any_single_string_function() {
        // setBaseURI(string), setBaseTokenURI(string), reveal(string)
        for sel in [
            [0x55, 0xf8, 0x04, 0xb3],
            [0x30, 0x17, 0x6e, 0x13],
            [0x4c, 0x26, 0x12, 0x47],
        ] {
            let uri = decode_reveal_uri(
                &string_call(sel, "ipfs://QmReveal/"),
                sel,
                DecodeKind::Typed,
            );
            assert_eq!(uri.unwrap(), "ipfs://QmReveal/");
        }
    }

    #[test]
    fn rejects_calldata_for_a_different_selector() {
        let input = string_call([0x55, 0xf8, 0x04, 0xb3], "ipfs://QmReveal/");
        assert!(decode_reveal_uri(&input, [0x30, 0x17, 0x6e, 0x13], DecodeKind::Typed).is_err());
        assert!(decode_reveal_uri(
            &Bytes::from(vec![0x55, 0xf8]),
            [0x55, 0xf8, 0x04, 0xb3],
            DecodeKind::Typed
        )
        .is_err());
    }

    #[test]
    fn typed_trims_whitespace() {
        let sel = [0x55, 0xf8, 0x04, 0xb3];
        let uri = decode_reveal_uri(
            &string_call(sel, " ipfs://QmReveal/ "),
            sel,
            DecodeKind::Typed,
        );
        assert_eq!(uri.unwrap(), "ipfs://QmReveal/");
    }

    #[test]
    fn multiconfigure_scans_for_the_uri() {
        let sel = [0x91, 0x1f, 0x45, 0x6b];
        let mut input = sel.to_vec();
        input.extend_from_slice(&[0u8; 64]);
        input.extend_from_slice(b"ipfs://QmReveal/");
        input.extend_from_slice(&[0u8; 16]);
        let uri = decode_reveal_uri(&Bytes::from(input), sel, DecodeKind::MultiConfigure);
        assert_eq!(uri.unwrap(), "ipfs://QmReveal/");
    }

    #[test]
    fn selector_bytes_validates_hex() {
        let ok = RevealSelector {
            selector: "0x55f804b3".into(),
            kind: DecodeKind::Typed,
        };
        assert_eq!(ok.selector_bytes().unwrap(), [0x55, 0xf8, 0x04, 0xb3]);
        for bad in ["", "0x", "0x55f804", "0x55f804b3ff", "0xzzzzzzzz"] {
            let s = RevealSelector {
                selector: bad.into(),
                kind: DecodeKind::Typed,
            };
            assert!(s.selector_bytes().is_err(), "{bad:?} should be rejected");
        }
    }
}
