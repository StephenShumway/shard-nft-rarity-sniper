//! QuickNode Streams webhook helpers.
//!
//! The desktop app exposes a single QuickNode webhook owned by
//! [`MonitorHub`](crate::monitor::MonitorHub), and QuickNode POSTs every confirmed
//! block (with full tx bodies) to it. This module holds that endpoint's
//! decode, auth and parse steps:
//!
//!   - [`decode_body`] gunzips the request body. QuickNode signs (and we parse)
//!     the *decompressed* payload, so it must be ungzipped first.
//!   - [`verify_signature`] authenticates with QuickNode's HMAC-SHA256 scheme:
//!     `HMAC(key = security token, msg = nonce ++ timestamp ++ body)`,
//!     hex-encoded, compared timing-safe against the `X-QN-Signature` header.
//!   - [`parse_stream_blocks`] parses the pushed `{data:[...]}` batch into
//!     per-block `(number, txs)` info, doing no matching. The hub matches each
//!     tx's `from` against its active deployer set itself.

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    http::{header::CONTENT_ENCODING, HeaderMap},
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::io::Read;

type HmacSha256 = Hmac<Sha256>;

// HeaderMap lookups are case-insensitive (names normalize to lowercase), so
// these lowercase keys match QuickNode's `X-QN-*` headers regardless of case.
const NONCE_HEADER: &str = "x-qn-nonce";
const TIMESTAMP_HEADER: &str = "x-qn-timestamp";
const SIGNATURE_HEADER: &str = "x-qn-signature";

/// Returns the request body, gunzipped if it was gzip-encoded. Checks both the
/// `Content-Encoding` header and the gzip magic bytes (some configs compress
/// without setting the header).
///
/// Run this before [`verify_signature`]: QuickNode signs the decompressed body.
pub(crate) fn decode_body(headers: &HeaderMap, body: &Bytes) -> Result<Vec<u8>> {
    let gzip_header = headers
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("gzip"));
    let gzip_magic = body.len() >= 2 && body[0] == 0x1f && body[1] == 0x8b;

    if gzip_header || gzip_magic {
        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .context("gzip decompress failed")?;
        Ok(out)
    } else {
        Ok(body.to_vec())
    }
}

/// QuickNode HMAC-SHA256 verification:
/// `HMAC(key = token, msg = nonce ++ timestamp ++ payload)`, hex-encoded,
/// compared timing-safe against `X-QN-Signature`. `payload` must be the
/// decompressed body from [`decode_body`].
pub(crate) fn verify_signature(token: &str, headers: &HeaderMap, payload: &[u8]) -> Result<()> {
    let nonce = header_str(headers, NONCE_HEADER).context("missing X-QN-Nonce header")?;
    let timestamp =
        header_str(headers, TIMESTAMP_HEADER).context("missing X-QN-Timestamp header")?;
    let signature_hex =
        header_str(headers, SIGNATURE_HEADER).context("missing X-QN-Signature header")?;

    let expected = hex::decode(signature_hex).context("X-QN-Signature is not valid hex")?;

    let mut mac = HmacSha256::new_from_slice(token.as_bytes())
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(nonce.as_bytes());
    mac.update(timestamp.as_bytes());
    mac.update(payload);

    // verify_slice performs the constant-time comparison internally.
    mac.verify_slice(&expected)
        .map_err(|_| anyhow::anyhow!("HMAC signature mismatch"))
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

#[derive(Deserialize)]
struct StreamPayload {
    #[serde(default)]
    data: Vec<StreamBlock>,
}

#[derive(Deserialize)]
struct StreamBlock {
    /// Block number, hex (`0x…`) or decimal. Absent on some payload shapes.
    number: Option<String>,
    #[serde(default)]
    transactions: Vec<StreamTx>,
}

#[derive(Deserialize)]
struct StreamTx {
    from: Option<String>,
    hash: Option<String>,
    input: Option<String>,
}

/// One block from a pushed QuickNode batch: its parsed number (if present) and
/// every tx as raw `(from, hash, input)`.
pub(crate) struct StreamBlockInfo {
    pub number: Option<u64>,
    pub txs: Vec<(String, String, String)>,
}

/// Parse a pushed QuickNode block batch into per-block info (number + raw txs),
/// doing no matching. [`MonitorHub`](crate::monitor::MonitorHub) uses this to
/// log block numbers and feed its `CandidateTx` broadcast (matching against the
/// active deployer set itself).
///
/// # Errors
/// Returns an error if the payload is not the expected `{data:[...]}` JSON.
pub(crate) fn parse_stream_blocks(payload: &[u8]) -> Result<Vec<StreamBlockInfo>> {
    let parsed: StreamPayload = serde_json::from_slice(payload).with_context(|| {
        let preview = String::from_utf8_lossy(&payload[..payload.len().min(200)]);
        format!("stream payload is not the expected {{data:[...]}} JSON (first 200B: {preview})")
    })?;

    let blocks = parsed
        .data
        .into_iter()
        .map(|block| StreamBlockInfo {
            number: block.number.as_deref().and_then(parse_block_number),
            txs: block
                .transactions
                .into_iter()
                .map(|tx| {
                    (
                        tx.from.unwrap_or_default(),
                        tx.hash.unwrap_or_default(),
                        tx.input.unwrap_or_else(|| "0x".to_string()),
                    )
                })
                .collect(),
        })
        .collect();
    Ok(blocks)
}

/// Parse a block number that may be `0x`-hex or plain decimal.
fn parse_block_number(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}
