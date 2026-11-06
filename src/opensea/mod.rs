//! OpenSea listings client: pages through a collection's best listings and
//! keeps the cheapest per token id. The signed order data needed to actually
//! fill a listing is fetched separately at buy time (see [`crate::seaport`]).

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::Deserialize;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use tracing::info;

const PAGE_LIMIT: u32 = 200;

const OPENSEA_API_BASE: &str = "https://api.opensea.io/api/v2";

/// One live listing: the cheapest per token id. The signed Seaport order
/// needed to fill it is fetched separately at buy time, keyed by `order_hash`.
#[derive(Debug, Clone)]
pub struct Listing {
    pub token_id: u64,
    pub price_eth: f64,
    pub price_wei: u128,
    pub order_hash: String,
}

#[derive(Debug, Deserialize)]
struct OpenSeaListingsResponse {
    listings: Vec<OpenSeaOrder>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenSeaOrder {
    order_hash: String,
    protocol_data: ProtocolData,
    price: OpenSeaPrice,
}

// Only the offered token id is read from the order's protocol data. Every other
// Seaport field is ignored (serde skips unknown fields), so an unusual value in
// one listing can't fail the whole page.
#[derive(Debug, Deserialize)]
struct ProtocolData {
    parameters: OrderParameters,
}

#[derive(Debug, Deserialize)]
struct OrderParameters {
    offer: Vec<OfferItem>,
}

#[derive(Debug, Deserialize)]
struct OfferItem {
    #[serde(rename = "identifierOrCriteria")]
    identifier_or_criteria: String,
}

#[derive(Debug, Deserialize)]
struct OpenSeaPrice {
    current: OpenSeaPriceCurrent,
}

#[derive(Debug, Deserialize)]
struct OpenSeaPriceCurrent {
    value: String,
    decimals: u8,
}

pub struct OpenSeaClient {
    client: Client,
    api_key: String,
    collection_slug: String,
}

impl OpenSeaClient {
    pub fn new(api_key: String, collection_slug: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            collection_slug,
        }
    }

    /// Fetch every page of the collection's best listings, keyed by token id
    /// (cheapest listing per token). Called once per reveal by the pipeline,
    /// concurrently with the metadata scrape.
    ///
    /// # Errors
    /// Fails the whole fetch if any page request fails or doesn't parse.
    pub async fn fetch_all_listings(&self) -> Result<HashMap<u64, Listing>> {
        let mut map: HashMap<u64, Listing> = HashMap::new();
        let mut cursor: Option<String> = None;
        let mut page = 0u32;
        let mut total_raw_fetched = 0usize;
        let base_url = format!(
            "{}/listings/collection/{}/best",
            OPENSEA_API_BASE, self.collection_slug
        );
        let limit_str = PAGE_LIMIT.to_string();

        loop {
            // Build query params via reqwest so the cursor (base64) is properly
            // URL-encoded. Manual `format!("&next={}", c)` would corrupt any
            // cursor containing `+` or `/` from standard base64.
            let mut params: Vec<(&str, &str)> = vec![("limit", &limit_str)];
            if let Some(c) = cursor.as_deref() {
                params.push(("next", c));
            }

            let resp = self
                .client
                .get(&base_url)
                .query(&params)
                .header("X-API-KEY", &self.api_key)
                .send()
                .await?;

            let status = resp.status();
            if !status.is_success() {
                // Don't return partial data, because a missing subset would silently
                // hide buyable targets. The pipeline fails the reveal cycle
                // instead.
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "OpenSea listings fetch failed (page {}, status {}): {}",
                    page + 1,
                    status,
                    body
                ));
            }

            let body: OpenSeaListingsResponse = resp.json().await?;
            let fetched = body.listings.len();
            total_raw_fetched += fetched;

            let mut bad_token_id = 0;
            let mut ok = 0;
            let mut dupes_replaced = 0;
            let mut dupes_kept = 0;

            for order in body.listings {
                match parse_listing(order) {
                    Some(listing) => {
                        // Explicitly keep the cheapest listing per token_id.
                        // OpenSea's /best endpoint usually returns ascending price,
                        // but we don't rely on that. A stale (higher-price) listing
                        // and a newer (lower-price) one for the same token can both
                        // appear, and we want the cheapest regardless of order.
                        match map.entry(listing.token_id) {
                            Entry::Occupied(mut occ) => {
                                if listing.price_wei < occ.get().price_wei {
                                    occ.insert(listing);
                                    dupes_replaced += 1;
                                } else {
                                    dupes_kept += 1;
                                }
                            }
                            Entry::Vacant(vac) => {
                                vac.insert(listing);
                            }
                        }
                        ok += 1;
                    }
                    None => {
                        bad_token_id += 1;
                    }
                }
            }

            page += 1;
            info!(
                "OpenSea page {}: {} fetched | {} parsed | {} skipped | {} dupes (cheaper kept) | {} dupes (existing was cheaper) (total: {})",
                page, fetched, ok, bad_token_id, dupes_replaced, dupes_kept, map.len()
            );

            // Termination: trust the cursor. A count-based check
            // (`fetched < PAGE_LIMIT`) is a fragile heuristic: if OpenSea ever
            // returns a short page mid-pagination, we'd quit early. An empty
            // page is the only count-based stop, as a guard against a
            // buggy/repeating cursor.
            if fetched == 0 {
                break;
            }
            match body.next {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }

        info!(
            "Fetched {} raw listings across {} page(s); {} unique tokens after dedup",
            total_raw_fetched,
            page,
            map.len()
        );
        Ok(map)
    }
}

fn parse_listing(order: OpenSeaOrder) -> Option<Listing> {
    let token_id: u64 = order
        .protocol_data
        .parameters
        .offer
        .first()?
        .identifier_or_criteria
        .parse()
        .ok()?;

    let price_wei: u128 = order.price.current.value.parse().ok()?;
    let decimals = order.price.current.decimals;
    let price_eth = price_wei as f64 / 10f64.powi(decimals as i32);

    Some(Listing {
        token_id,
        price_eth,
        price_wei,
        order_hash: order.order_hash,
    })
}
