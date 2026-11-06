//! Seaport order preparation and the batched `snipe()` send path.
//!
//! [`SeaportExecutor::buy_batch`] fetches OpenSea fulfillment data for each
//! target, ABI-encodes one Seaport `fulfillAdvancedOrder` call per order, wraps
//! them all in a single `RaritySniper.snipe()` transaction, signs it offline and
//! hands it to [`RelayClient`] for bundle submission. `OrderFilled` events in
//! the receipt map back to per-target results.

use alloy::{
    eips::eip2718::Encodable2718,
    network::{EthereumWallet, NetworkTransactionBuilder, TransactionBuilder},
    primitives::{Address, Bytes, FixedBytes, B256, U256},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::{TransactionReceipt, TransactionRequest},
    signers::local::PrivateKeySigner,
    sol,
    sol_types::{SolCall, SolEvent},
};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::opensea::Listing;
use crate::relay::RelayClient;

const SEAPORT_ADDRESS: &str = "0x0000000000000068F116a894984e2DB1123eB395";
const OPENSEA_API_BASE: &str = "https://api.opensea.io/api/v2";
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(30);
const RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Gas headroom for a batched snipe. With keep-winners semantics the gas used
/// scales with how many orders *actually* fill, which can exceed a current-state
/// `eth_estimateGas` (an order stale at estimate time may be live at inclusion).
/// So we floor the limit at "every order fills" and add a margin. You only pay
/// for gas used, so over-provisioning the limit is free insurance against OOG.
const BATCH_BASE_GAS: u64 = 80_000; // snipe loop + tip + refund + calldata
const PER_ORDER_GAS: u64 = 350_000; // generous upper bound for one Seaport fill

sol! {
    #[allow(missing_docs)]
    #[sol(rpc)]
    contract Seaport {
        struct OfferItem {
            uint8 itemType;
            address token;
            uint256 identifierOrCriteria;
            uint256 startAmount;
            uint256 endAmount;
        }

        struct ConsiderationItem {
            uint8 itemType;
            address token;
            uint256 identifierOrCriteria;
            uint256 startAmount;
            uint256 endAmount;
            address recipient;
        }

        struct OrderParameters {
            address offerer;
            address zone;
            OfferItem[] offer;
            ConsiderationItem[] consideration;
            uint8 orderType;
            uint256 startTime;
            uint256 endTime;
            bytes32 zoneHash;
            uint256 salt;
            bytes32 conduitKey;
            uint256 totalOriginalConsiderationItems;
        }

        struct AdvancedOrder {
            OrderParameters parameters;
            uint120 numerator;
            uint120 denominator;
            bytes signature;
            bytes extraData;
        }

        struct CriteriaResolver {
            uint256 orderIndex;
            uint8 side;
            uint256 index;
            uint256 identifier;
            bytes32[] criteriaProof;
        }

        function fulfillAdvancedOrder(
            AdvancedOrder calldata advancedOrder,
            CriteriaResolver[] calldata criteriaResolvers,
            bytes32 fulfillerConduitKey,
            address recipient
        ) external payable returns (bool fulfilled);
    }
}

sol! {
    #[allow(missing_docs)]
    contract RaritySniper {
        function snipe(
            address[] contractAddresses,
            bytes[] calls,
            uint256[] values,
            uint256 validatorTip
        ) external payable;

        event OrderFilled(uint256 index);
    }
}

// ── OpenSea fulfillment_data types ────────────────────────────────────────────

#[derive(Serialize)]
struct FulfillmentRequest<'a> {
    listing: FulfillmentListing<'a>,
    fulfiller: FulfillmentFulfiller<'a>,
}

#[derive(Serialize)]
struct FulfillmentListing<'a> {
    hash: &'a str,
    chain: &'a str,
    protocol_address: &'a str,
}

#[derive(Serialize)]
struct FulfillmentFulfiller<'a> {
    address: &'a str,
}

#[derive(Deserialize)]
struct FulfillmentResponse {
    fulfillment_data: FulfillmentData,
}

#[derive(Deserialize)]
struct FulfillmentData {
    transaction: FulfillmentTransaction,
}

#[derive(Deserialize)]
struct FulfillmentTransaction {
    value: String,
    input_data: InputData,
}

#[derive(Deserialize)]
struct InputData {
    #[serde(rename = "advancedOrder")]
    advanced_order: AdvancedOrderData,
    #[serde(rename = "fulfillerConduitKey")]
    fulfiller_conduit_key: String,
    // `recipient` from OpenSea is intentionally ignored. When we fetch with the
    // contract as fulfiller, OpenSea returns recipient=contract, but we force
    // the NFT to our EOA in `prepare_order` instead (the contract has no
    // ERC721 rescue).
}

#[derive(Deserialize)]
struct AdvancedOrderData {
    parameters: OrderParametersData,
    #[serde(deserialize_with = "deserialize_u64_from_value")]
    numerator: u64,
    #[serde(deserialize_with = "deserialize_u64_from_value")]
    denominator: u64,
    signature: String,
    #[serde(rename = "extraData")]
    extra_data: String,
}

#[derive(Deserialize)]
struct OrderParametersData {
    offerer: String,
    zone: String,
    offer: Vec<OfferItemData>,
    consideration: Vec<ConsiderationItemData>,
    #[serde(rename = "orderType")]
    order_type: serde_json::Value, // int32 in orders, string in input_data
    #[serde(rename = "startTime")]
    start_time: serde_json::Value, // string
    #[serde(rename = "endTime")]
    end_time: serde_json::Value, // string
    #[serde(rename = "zoneHash")]
    zone_hash: String,
    salt: serde_json::Value, // string
    #[serde(rename = "conduitKey")]
    conduit_key: String,
    #[serde(rename = "totalOriginalConsiderationItems")]
    total_original_consideration_items: serde_json::Value, // int32
}

#[derive(Deserialize)]
struct OfferItemData {
    #[serde(rename = "itemType")]
    item_type: serde_json::Value, // int32 in orders, string in input_data
    token: String,
    #[serde(rename = "identifierOrCriteria")]
    identifier_or_criteria: String,
    #[serde(rename = "startAmount")]
    start_amount: String,
    #[serde(rename = "endAmount")]
    end_amount: String,
}

#[derive(Deserialize)]
struct ConsiderationItemData {
    #[serde(rename = "itemType")]
    item_type: serde_json::Value, // int32 in orders, string in input_data
    token: String,
    #[serde(rename = "identifierOrCriteria")]
    identifier_or_criteria: String,
    #[serde(rename = "startAmount")]
    start_amount: String,
    #[serde(rename = "endAmount")]
    end_amount: String,
    recipient: String,
}

// ── Batch types ─────────────────────────────────────────────────────────────

/// One target listing for a batched snipe. `rank` is carried for log lines only.
pub struct BatchOrder {
    pub listing: Listing,
    pub rank: u64,
}

/// Disposition of one order after a batched snipe, index-aligned with the
/// [`SeaportExecutor::buy_batch`] input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrderResult {
    /// Filled on-chain (the contract emitted `OrderFilled` for it).
    Filled,
    /// Sent in the batch but did not fill (skipped on-chain, or the tx reverted).
    NotFilled,
    /// OpenSea fulfillment fetch/encode failed, so it was never sent.
    Unprepared,
}

/// Result of a batched [`SeaportExecutor::buy_batch`].
pub struct BatchSnipeResult {
    /// The snipe tx hash, or `None` if no order could be prepared (nothing sent).
    pub tx_hash: Option<B256>,
    /// Block the tx landed in, if a receipt was obtained. `None` here with a
    /// `Some` `tx_hash` means the bundle was sent but never mined within the
    /// receipt timeout (an inclusion miss, not an on-chain revert).
    pub block_number: Option<u64>,
    /// Whether the snipe tx itself reverted (e.g. every order failed on-chain).
    pub tx_reverted: bool,
    /// Per-target disposition, index-aligned with the `buy_batch` input.
    pub results: Vec<OrderResult>,
    /// The block the bundle targeted (`head + 1`). Compare with `block_number`
    /// to see how many blocks past the target it actually landed (the blank
    /// block-number variant can float forward when send latency crosses a slot).
    pub target_block: u64,
    /// Relay POSTs that returned 2xx, out of those attempted. `relay_accepted
    /// == 0` ⇒ no relay took the bundle (check relay auth/config).
    /// `relay_accepted > 0` with `block_number == None` ⇒ sent but never
    /// included (lost the block auction in the target window).
    pub relay_accepted: usize,
    pub relay_attempted: usize,
}

// ── Executor ──────────────────────────────────────────────────────────────────

/// Builds, signs and submits the batched `snipe()` transaction for one buying
/// wallet. Built once per task and shared behind an `Arc`.
pub struct SeaportExecutor {
    http_client: Client,
    opensea_api_key: String,
    wallet_addr: Address,
    wallet: EthereumWallet,
    chain_id: u64,
    seaport_address: Address,
    sniper_contract: Address,
    validator_tip_wei: U256,
    relay: RelayClient,
    provider: DynProvider,
}

impl SeaportExecutor {
    pub async fn new(
        rpc_url: &str,
        private_key: &str,
        opensea_api_key: String,
        relays: Vec<String>,
        sniper_contract: Address,
        validator_tip_wei: U256,
    ) -> Result<Self> {
        let signer: PrivateKeySigner = private_key.parse()?;
        let wallet_addr = signer.address();
        let seaport_address: Address = SEAPORT_ADDRESS.parse()?;

        // One reqwest client shared by OpenSea calls and relay bundle posts so
        // connections to all hosts stay warm in a single pool.
        let http_client = Client::new();
        let relay = RelayClient::new(http_client.clone(), signer.clone(), relays);

        // The wallet signs the snipe tx offline (we never broadcast to the
        // mempool). Gas/fees are estimated per snipe via the provider so we
        // don't hand-set them. chain_id is fixed, so cache it once here.
        let wallet = EthereumWallet::from(signer);
        let provider = ProviderBuilder::new()
            .wallet(wallet.clone())
            .connect(rpc_url)
            .await?
            .erased();
        let chain_id = provider
            .get_chain_id()
            .await
            .context("get_chain_id failed")?;

        Ok(Self {
            http_client,
            opensea_api_key,
            wallet_addr,
            wallet,
            chain_id,
            seaport_address,
            sniper_contract,
            validator_tip_wei,
            relay,
            provider,
        })
    }

    /// Fetch the wallet's confirmed nonce and the latest block height
    /// concurrently. Called once per reveal, just before the buy. Every target
    /// goes into one `snipe()` at this nonce, and `head + 1` is the explicit
    /// bundle block target.
    pub async fn fetch_nonce_and_head(&self) -> Result<(u64, u64)> {
        let (nonce, head) = tokio::join!(
            async { self.provider.get_transaction_count(self.wallet_addr).await },
            async { self.provider.get_block_number().await },
        );
        Ok((
            nonce.context("get_transaction_count failed")?,
            head.context("get_block_number failed")?,
        ))
    }

    /// Build the per-order Seaport call for one listing: fetch OpenSea
    /// fulfillment data (fulfiller = the contract, so the signed zone binds to
    /// it), then ABI-encode `fulfillAdvancedOrder` with the NFT recipient forced
    /// to our EOA. Returns the calldata + the msg.value the order needs. Used by
    /// [`buy_batch`](Self::buy_batch) to prepare each order in the snipe.
    async fn prepare_order(&self, listing: &Listing) -> Result<(Bytes, U256)> {
        let fulfillment = self
            .fetch_fulfillment_data(listing, &self.sniper_contract.to_checksum(None))
            .await?;
        let input = &fulfillment.fulfillment_data.transaction.input_data;

        // Fall back to the listing price if OpenSea returns a malformed value.
        let raw_value = &fulfillment.fulfillment_data.transaction.value;
        let value: U256 = raw_value.parse().unwrap_or_else(|e| {
            warn!(
                "OpenSea returned unparseable value {:?} ({}); falling back to listing price_wei",
                raw_value, e
            );
            U256::from(listing.price_wei)
        });

        let advanced_order = self.build_advanced_order(&input.advanced_order)?;
        let fulfiller_conduit_key: FixedBytes<32> = input.fulfiller_conduit_key.parse()?;

        // Signed zone is bound to the contract (fulfiller), but the NFT is delivered
        // to `recipient` = our EOA so it lands in the wallet, not the contract.
        let seaport_calldata = Seaport::fulfillAdvancedOrderCall {
            advancedOrder: advanced_order,
            criteriaResolvers: vec![],
            fulfillerConduitKey: fulfiller_conduit_key,
            recipient: self.wallet_addr,
        }
        .abi_encode();

        Ok((Bytes::from(seaport_calldata), value))
    }

    /// Buy many listings in a **single** `snipe()` transaction: one nonce, one
    /// block. Orders are prepared concurrently. Any that fail OpenSea fulfillment
    /// are dropped (reported `Unprepared`) and the rest are batched. With the
    /// keep-winners contract a stale order is skipped on-chain rather than
    /// reverting the batch, so each target comes back as `Filled`, `NotFilled`,
    /// or `Unprepared`, index-aligned with `targets`.
    ///
    /// `head` is the current block height, and the bundle targets `head + 1`.
    ///
    /// # Errors
    /// Only if fee estimation or signing fails. A missing receipt is not an
    /// error: it comes back as `block_number: None` (an inclusion miss).
    pub async fn buy_batch(
        &self,
        targets: &[BatchOrder],
        nonce: u64,
        head: u64,
    ) -> Result<BatchSnipeResult> {
        let n = targets.len();
        let mut results = vec![OrderResult::Unprepared; n];

        // Prepare every order concurrently (independent OpenSea fetches + encode).
        let prepared =
            futures::future::join_all(targets.iter().map(|t| self.prepare_order(&t.listing))).await;

        // Keep the successes and remember which original target each maps to.
        let mut addrs: Vec<Address> = Vec::with_capacity(n);
        let mut calls: Vec<Bytes> = Vec::with_capacity(n);
        let mut values: Vec<U256> = Vec::with_capacity(n);
        let mut sent_to_target: Vec<usize> = Vec::with_capacity(n);
        let mut sum_values = U256::ZERO;
        for (i, prep) in prepared.into_iter().enumerate() {
            match prep {
                Ok((calldata, value)) => {
                    addrs.push(self.seaport_address);
                    calls.push(calldata);
                    values.push(value);
                    sum_values += value;
                    sent_to_target.push(i);
                }
                Err(e) => warn!("rank {} order prep failed: {}", targets[i].rank, e),
            }
        }

        if calls.is_empty() {
            return Ok(BatchSnipeResult {
                tx_hash: None,
                block_number: None,
                tx_reverted: false,
                results,
                target_block: head + 1,
                relay_accepted: 0,
                relay_attempted: 0,
            });
        }
        let sent = calls.len() as u64;

        // One snipe() over all prepared orders.
        let snipe_calldata = RaritySniper::snipeCall {
            contractAddresses: addrs,
            calls,
            values,
            validatorTip: self.validator_tip_wei,
        }
        .abi_encode();
        let total_value = sum_values + self.validator_tip_wei;

        let base_tx = TransactionRequest::default()
            .with_from(self.wallet_addr)
            .with_to(self.sniper_contract)
            .with_value(total_value)
            .with_input(snipe_calldata)
            .with_nonce(nonce)
            .with_chain_id(self.chain_id);

        // Fees + gas concurrently. Gas is floored at "every order fills" then
        // given a margin, so a partial current-state estimate can't
        // under-provision and OOG the tx if more orders fill at inclusion.
        let (gas_est, fees) = tokio::join!(
            async { self.provider.estimate_gas(base_tx.clone()).await },
            async { self.provider.estimate_eip1559_fees().await },
        );
        let fees = fees.map_err(|e| anyhow::anyhow!("fee estimation failed: {}", e))?;
        let gas_floor = BATCH_BASE_GAS + PER_ORDER_GAS * sent;
        let gas_limit = match gas_est {
            Ok(g) => g.max(gas_floor).saturating_mul(12) / 10,
            Err(e) => {
                warn!("batch gas estimate failed ({e}); using floor {gas_floor}");
                gas_floor
            }
        };

        let envelope = base_tx
            .with_gas_limit(gas_limit)
            .with_max_fee_per_gas(fees.max_fee_per_gas)
            .with_max_priority_fee_per_gas(fees.max_priority_fee_per_gas)
            .build(&self.wallet)
            .await
            .map_err(|e| anyhow::anyhow!("tx signing failed: {}", e))?;

        let tx_hash = *envelope.tx_hash();
        let raw_tx = format!("0x{}", hex::encode(envelope.encoded_2718()));
        let target_block = head + 1;

        info!("Purchasing {sent} order(s) in one snipe (tx {tx_hash:?})");
        let stats = self.relay.broadcast(&raw_tx, head).await;
        info!(
            "Bundle broadcast: {}/{} relay POST(s) accepted, target block {target_block}",
            stats.accepted, stats.attempted
        );

        // Every sent order starts NotFilled, and an OrderFilled log promotes it to
        // Filled once we have a receipt. Set here (before the receipt wait) so
        // the no-receipt path below still returns accurate dispositions.
        for &ti in &sent_to_target {
            results[ti] = OrderResult::NotFilled;
        }

        // No receipt (timeout elapsed, or a receipt poll errored) ⇒ treat the
        // bundle as never mined (typically it lost the auction in its target
        // blocks). Return what we know instead of erroring: relay acceptance +
        // a `None` block let the caller log an *inclusion* miss, distinct from
        // an on-chain revert (which would yield a receipt with a failed status).
        let receipt = match self.await_receipt(tx_hash).await {
            Ok(r) => r,
            Err(e) => {
                warn!("no receipt for {tx_hash:?} within timeout: {e}");
                return Ok(BatchSnipeResult {
                    tx_hash: Some(tx_hash),
                    block_number: None,
                    tx_reverted: false,
                    results,
                    target_block,
                    relay_accepted: stats.accepted,
                    relay_attempted: stats.attempted,
                });
            }
        };
        let block_number = receipt.block_number;
        let tx_reverted = !receipt.status();

        if !tx_reverted {
            let topic0 = RaritySniper::OrderFilled::SIGNATURE_HASH;
            for log in receipt.logs() {
                if log.inner.address != self.sniper_contract
                    || log.inner.data.topics().first() != Some(&topic0)
                {
                    continue;
                }
                // OrderFilled(uint256 index) is non-indexed, so the index is the
                // last 8 bytes of the 32-byte data word (it's always < sent).
                let data = log.inner.data.data.as_ref();
                if data.len() == 32 {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(&data[24..32]);
                    let sent_idx = u64::from_be_bytes(buf) as usize;
                    if let Some(&ti) = sent_to_target.get(sent_idx) {
                        results[ti] = OrderResult::Filled;
                    }
                }
            }
        }

        debug!("Batch snipe receipt: block={block_number:?} reverted={tx_reverted} sent={sent}");

        Ok(BatchSnipeResult {
            tx_hash: Some(tx_hash),
            block_number,
            tx_reverted,
            results,
            target_block,
            relay_accepted: stats.accepted,
            relay_attempted: stats.attempted,
        })
    }

    /// Poll the provider for the receipt of `tx_hash` until it lands or
    /// `RECEIPT_TIMEOUT` elapses. A failed poll ends the wait early with that
    /// error.
    async fn await_receipt(&self, tx_hash: B256) -> Result<TransactionReceipt> {
        tokio::time::timeout(RECEIPT_TIMEOUT, async {
            loop {
                if let Some(receipt) = self
                    .provider
                    .get_transaction_receipt(tx_hash)
                    .await
                    .context("get_transaction_receipt failed")?
                {
                    return Ok::<_, anyhow::Error>(receipt);
                }
                tokio::time::sleep(RECEIPT_POLL_INTERVAL).await;
            }
        })
        .await
        .with_context(|| format!("timed out waiting for receipt of {tx_hash:?}"))?
    }

    async fn fetch_fulfillment_data(
        &self,
        listing: &Listing,
        fulfiller: &str,
    ) -> Result<FulfillmentResponse> {
        let body = FulfillmentRequest {
            listing: FulfillmentListing {
                hash: &listing.order_hash,
                chain: "ethereum",
                protocol_address: SEAPORT_ADDRESS,
            },
            fulfiller: FulfillmentFulfiller { address: fulfiller },
        };

        let resp = self
            .http_client
            .post(format!("{}/listings/fulfillment_data", OPENSEA_API_BASE))
            .header("X-API-KEY", &self.opensea_api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenSea fulfillment_data failed {}: {}", status, text);
        }

        Ok(resp.json::<FulfillmentResponse>().await?)
    }

    fn build_advanced_order(&self, data: &AdvancedOrderData) -> Result<Seaport::AdvancedOrder> {
        let params = &data.parameters;

        let offer = params
            .offer
            .iter()
            .map(|o| -> Result<Seaport::OfferItem> {
                Ok(Seaport::OfferItem {
                    itemType: parse_u8_json(&o.item_type).context("invalid offer.itemType")?,
                    token: o.token.parse().context("invalid offer.token address")?,
                    identifierOrCriteria: parse_u256_str(&o.identifier_or_criteria)
                        .context("invalid offer.identifierOrCriteria")?,
                    startAmount: parse_u256_str(&o.start_amount)
                        .context("invalid offer.startAmount")?,
                    endAmount: parse_u256_str(&o.end_amount).context("invalid offer.endAmount")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let consideration = params
            .consideration
            .iter()
            .map(|c| -> Result<Seaport::ConsiderationItem> {
                Ok(Seaport::ConsiderationItem {
                    itemType: parse_u8_json(&c.item_type)
                        .context("invalid consideration.itemType")?,
                    token: c
                        .token
                        .parse()
                        .context("invalid consideration.token address")?,
                    identifierOrCriteria: parse_u256_str(&c.identifier_or_criteria)
                        .context("invalid consideration.identifierOrCriteria")?,
                    startAmount: parse_u256_str(&c.start_amount)
                        .context("invalid consideration.startAmount")?,
                    endAmount: parse_u256_str(&c.end_amount)
                        .context("invalid consideration.endAmount")?,
                    recipient: c
                        .recipient
                        .parse()
                        .context("invalid consideration.recipient")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let order_params = Seaport::OrderParameters {
            offerer: params.offerer.parse().context("invalid offerer address")?,
            zone: params.zone.parse().context("invalid zone address")?,
            offer,
            consideration,
            orderType: parse_u8_json(&params.order_type).context("invalid orderType")?,
            startTime: parse_u256_json(&params.start_time).context("invalid startTime")?,
            endTime: parse_u256_json(&params.end_time).context("invalid endTime")?,
            zoneHash: params.zone_hash.parse().context("invalid zoneHash")?,
            salt: parse_u256_json(&params.salt).context("invalid salt")?,
            conduitKey: params.conduit_key.parse().context("invalid conduitKey")?,
            totalOriginalConsiderationItems: parse_u256_json(
                &params.total_original_consideration_items,
            )
            .context("invalid totalOriginalConsiderationItems")?,
        };

        // uint120 in alloy sol! macro maps to alloy::primitives::Uint<120, 2>
        type U120 = alloy::primitives::Uint<120, 2>;

        Ok(Seaport::AdvancedOrder {
            parameters: order_params,
            numerator: U120::from(data.numerator),
            denominator: U120::from(data.denominator),
            signature: hex_to_bytes(&data.signature)?,
            extraData: hex_to_bytes(&data.extra_data)?,
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_u256_str(s: &str) -> Result<U256> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        U256::from_str_radix(hex, 16).map_err(|e| anyhow::anyhow!("invalid hex u256 {s:?}: {e}"))
    } else {
        s.parse::<U256>()
            .map_err(|e| anyhow::anyhow!("invalid decimal u256 {s:?}: {e}"))
    }
}

fn parse_u256_json(val: &serde_json::Value) -> Result<U256> {
    match val {
        serde_json::Value::String(s) => parse_u256_str(s),
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(U256::from)
            .ok_or_else(|| anyhow::anyhow!("number {n} does not fit in u64")),
        other => Err(anyhow::anyhow!("expected string or number, got {other:?}")),
    }
}

fn hex_to_bytes(hex: &str) -> Result<Bytes> {
    let hex = hex.trim_start_matches("0x");
    Ok(Bytes::from(hex::decode(hex)?))
}

fn deserialize_u64_from_value<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Number(n) => {
            n.as_u64().ok_or_else(|| D::Error::custom("invalid number"))
        }
        serde_json::Value::String(s) => s
            .parse::<u64>()
            .map_err(|_| D::Error::custom(format!("invalid string: {}", s))),
        _ => Err(D::Error::custom("expected string or number")),
    }
}

fn parse_u8_json(val: &serde_json::Value) -> Result<u8> {
    match val {
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(|| anyhow::anyhow!("u8 out of range: {n}")),
        serde_json::Value::String(s) => s
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid u8 string {s:?}: {e}")),
        other => Err(anyhow::anyhow!(
            "expected number or string for u8, got {other:?}"
        )),
    }
}
