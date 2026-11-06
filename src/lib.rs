//! Core library for Shard, an Ethereum NFT reveal sniper.
//!
//! [`monitor::MonitorHub`] watches each task's deployer through the bloxroute
//! mempool stream and a QuickNode Streams webhook. [`pipeline`] services a
//! detected reveal: it scrapes metadata over IPFS, scores rarity, fetches
//! OpenSea listings, and buys the rarest listed tokens in one
//! `RaritySniper.snipe()` transaction sent as builder-relay bundles.
//! Configuration lives in JSON stores ([`settings`], [`wallet`], [`task`])
//! that the desktop app in `src-tauri/` manages.

pub mod diagnostics;
pub mod discord;
pub mod ipfs;
pub mod mempool;
pub mod monitor;
pub mod opensea;
pub mod pipeline;
pub mod rarity;
pub mod relay;
pub mod reporter;
pub mod seaport;
pub mod settings;
pub mod store;
pub mod streamwatcher;
pub mod task;
pub mod wallet;
