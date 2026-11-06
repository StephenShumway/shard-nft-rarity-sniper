//! OpenRarity-style rarity scoring and ranking over scraped token metadata.

use crate::ipfs::TokenData;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct ScoredToken {
    pub token_id: u64,
    pub rarity_score: f64,
    pub rank: u64,
    pub trait_scores: Vec<TraitScore>,
}

#[derive(Debug, Clone)]
pub struct TraitScore {
    pub trait_type: String,
    pub value: String,
    pub frequency: f64, // 0.0 to 1.0
    pub score: f64,     // information content: -log2(frequency)
}

pub struct RarityEngine;

impl RarityEngine {
    /// Score all tokens with the OpenRarity Information Content method and
    /// return them ranked rarest-first.
    ///
    /// Each trait contributes `-log2(P(value))` bits of information, where
    /// `P(value)` is that value's frequency in the collection. A token's score
    /// is the sum across every trait type, including an implicit null trait
    /// (recorded as value `"None"`) for any type the token lacks. Missing a
    /// common trait is itself informative, and tokens with more traits aren't
    /// artificially inflated: every token is scored over the same trait-type
    /// universe.
    ///
    /// `rarity_score` is OpenRarity's normalized score `I(x) / E[I(x)]`: the
    /// information content `I(x) = Σ -log2(P(trait_i))` divided by the
    /// collection's mean information content `E[I(x)]`. That divisor is a
    /// collection-wide constant, so it never changes rank order, but it puts
    /// scores on OpenRarity's published scale and keeps them stable under a
    /// coverage-capped (partial) scrape, since numerator and denominator are
    /// drawn from the same sample. The per-entry `trait_scores[*].score` stay
    /// as raw per-trait IC, so their sum is `I(x)` (pre-normalization), not the
    /// final `rarity_score`.
    pub fn score(collection: &[TokenData]) -> Vec<ScoredToken> {
        let total = collection.len() as f64;
        if collection.is_empty() {
            return Vec::new();
        }

        // Capacity hints only, to cut rehashing in the count pass: the first
        // token's trait count stands in for the per-token average, and distinct
        // (type, value) pairs are estimated at one per 8 trait occurrences.
        let avg_traits = collection
            .first()
            .map_or(8, |t| t.metadata.attributes.len())
            .max(1);
        let estimated_pairs = (collection.len() * avg_traits / 8).max(64);

        let mut value_freq: HashMap<(String, String), u64> =
            HashMap::with_capacity(estimated_pairs);
        let mut type_present: HashMap<String, u64> = HashMap::with_capacity(avg_traits * 2);

        for token in collection {
            let mut seen_types: HashSet<&str> = HashSet::with_capacity(avg_traits);
            for attr in &token.metadata.attributes {
                let value = value_as_key(&attr.value);
                let key = (attr.trait_type.to_lowercase(), value.to_lowercase());
                *value_freq.entry(key).or_insert(0) += 1;
                if seen_types.insert(attr.trait_type.as_str()) {
                    *type_present.entry(attr.trait_type.clone()).or_insert(0) += 1;
                }
            }
        }

        let all_trait_types: HashSet<String> = type_present.keys().cloned().collect();

        let mut scored: Vec<ScoredToken> = collection
            .iter()
            .map(|token| {
                let present_types: HashSet<&str> = token
                    .metadata
                    .attributes
                    .iter()
                    .map(|a| a.trait_type.as_str())
                    .collect();

                let mut trait_scores: Vec<TraitScore> = token
                    .metadata
                    .attributes
                    .iter()
                    .map(|attr| {
                        let value = value_as_key(&attr.value);
                        let key = (attr.trait_type.to_lowercase(), value.to_lowercase());
                        let count = *value_freq.get(&key).unwrap_or(&1) as f64;
                        let frequency = count / total;
                        TraitScore {
                            trait_type: attr.trait_type.clone(),
                            value,
                            frequency,
                            score: -frequency.log2(),
                        }
                    })
                    .collect();

                // Null trait: each missing type scores as "None" at the
                // frequency with which tokens lack that type.
                for trait_type in &all_trait_types {
                    if !present_types.contains(trait_type.as_str()) {
                        let present = *type_present.get(trait_type).unwrap_or(&0) as f64;
                        let missing = total - present;
                        if missing > 0.0 {
                            let frequency = missing / total;
                            trait_scores.push(TraitScore {
                                trait_type: trait_type.clone(),
                                value: "None".to_string(),
                                frequency,
                                score: -frequency.log2(),
                            });
                        }
                    }
                }

                // I(x): total information content across all traits
                // (normalized below).
                let rarity_score: f64 = trait_scores.iter().map(|t| t.score).sum();

                ScoredToken {
                    token_id: token.token_id,
                    rarity_score,
                    rank: 0,
                    trait_scores,
                }
            })
            .collect();

        // Normalize by E[I(x)], the mean information content over this
        // (possibly coverage-capped) sample, giving I(x)/E[I(x)]. Dividing by a
        // positive constant leaves rank order unchanged.
        let mean_ic = scored.iter().map(|t| t.rarity_score).sum::<f64>() / scored.len() as f64;
        if mean_ic > 0.0 {
            for token in &mut scored {
                token.rarity_score /= mean_ic;
            }
        }

        // Rank: 1 = rarest. total_cmp avoids NaN panic.
        scored.sort_by(|a, b| b.rarity_score.total_cmp(&a.rarity_score));
        for (i, token) in scored.iter_mut().enumerate() {
            token.rank = (i + 1) as u64;
        }

        scored
    }
}

/// Extract a clean string key from a JSON Value. Strings are unwrapped (no surrounding quotes).
/// Other scalars are formatted as-is.
fn value_as_key(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}
