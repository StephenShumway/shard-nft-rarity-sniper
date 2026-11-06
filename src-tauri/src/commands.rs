//! Tauri command surface: the entire backend API the frontend calls via
//! `invoke`. Commands are thin: they validate, touch shared state, persist, and
//! return serializable results. Heavy work lives in the core lib / [`crate::tasks`].

use serde::Serialize;
use tauri::State;

use rarity_sniper::diagnostics::{ping_endpoint, rpc_latency, PingResult, RpcResult};
use rarity_sniper::discord;
use rarity_sniper::settings::Settings;
use rarity_sniper::task::{self, TaskConfig, TaskStage};
use rarity_sniper::wallet::{self, WalletEntry};

use crate::state::AppState;
use crate::tasks;

// ── Settings ────────────────────────────────────────────────────────────────

/// Settings for the UI, with the hidden `sniper_contract` blanked.
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.settings.read().await.for_ui())
}

/// Persist UI-edited settings, keeping the stored `sniper_contract`. Running
/// tasks keep the settings they started with, and the shared monitor reads its
/// bloxroute/webhook config only when it starts (with the first running task).
#[tauri::command]
pub async fn save_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    let mut guard = state.settings.write().await;
    let merged = guard.merge_from_ui(settings);
    merged.save().map_err(|e| e.to_string())?;
    *guard = merged;
    Ok(())
}

#[tauri::command]
pub async fn test_endpoint(url: String) -> Result<PingResult, String> {
    if url.trim().is_empty() {
        return Err("url is empty".to_string());
    }
    Ok(ping_endpoint(url.trim()).await)
}

#[tauri::command]
pub async fn test_rpc(url: String) -> Result<RpcResult, String> {
    if url.trim().is_empty() {
        return Err("url is empty".to_string());
    }
    Ok(rpc_latency(url.trim()).await)
}

#[tauri::command]
pub async fn test_discord(url: String) -> Result<(), String> {
    if url.trim().is_empty() {
        return Err("discord webhook url is empty".to_string());
    }
    discord::send_test(url.trim())
        .await
        .map_err(|e| e.to_string())
}

// ── Wallets ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_wallets(state: State<'_, AppState>) -> Result<Vec<WalletEntry>, String> {
    Ok(state.wallets.read().await.wallets.clone())
}

#[tauri::command]
pub async fn add_wallet(
    state: State<'_, AppState>,
    private_key: String,
    label: Option<String>,
) -> Result<WalletEntry, String> {
    let mut wallets = state.wallets.write().await;
    let entry = wallets
        .add(&private_key, label)
        .map_err(|e| e.to_string())?;
    wallets.save().map_err(|e| e.to_string())?;
    Ok(entry)
}

#[tauri::command]
pub async fn remove_wallet(state: State<'_, AppState>, address: String) -> Result<(), String> {
    let mut wallets = state.wallets.write().await;
    wallets.remove(&address);
    wallets.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn fetch_balance(state: State<'_, AppState>, address: String) -> Result<f64, String> {
    let rpc = state.settings.read().await.eth_rpc_url.clone();
    if rpc.trim().is_empty() {
        return Err("set the RPC URL in Settings first".to_string());
    }
    let balance = wallet::fetch_balance_eth(&rpc, &address)
        .await
        .map_err(|e| e.to_string())?;
    let mut wallets = state.wallets.write().await;
    wallets.set_balance(&address, balance);
    wallets.save().map_err(|e| e.to_string())?;
    Ok(balance)
}

// ── Tasks ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TaskStatus {
    pub slug: String,
    pub running: bool,
    pub stage: String,
}

#[tauri::command]
pub async fn list_tasks() -> Result<Vec<TaskConfig>, String> {
    Ok(task::list())
}

#[tauri::command]
pub async fn save_task(task: TaskConfig) -> Result<(), String> {
    task.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_task(state: State<'_, AppState>, slug: String) -> Result<(), String> {
    state.tasks.stop(&slug);
    state.stages.remove(&slug);
    task::delete(&slug).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn start_task(state: State<'_, AppState>, slug: String) -> Result<(), String> {
    tasks::start(&state, &slug).await
}

#[tauri::command]
pub async fn stop_task(state: State<'_, AppState>, slug: String) -> Result<(), String> {
    if !state.tasks.stop(&slug) {
        return Err(format!("task {slug} is not running"));
    }
    Ok(())
}

/// Combined snapshot for the Tasks tab on load: every saved task's slug with
/// its running flag and current stage. A task with no cached stage reports
/// `monitoring` if running, else `idle`.
#[tauri::command]
pub async fn task_statuses(state: State<'_, AppState>) -> Result<Vec<TaskStatus>, String> {
    let statuses = task::list()
        .into_iter()
        .map(|cfg| {
            let running = state.tasks.is_running(&cfg.opensea_slug);
            let stage = state
                .stages
                .get(&cfg.opensea_slug)
                .map(|s| *s)
                .unwrap_or(if running {
                    TaskStage::Monitoring
                } else {
                    TaskStage::Idle
                });
            TaskStatus {
                slug: cfg.opensea_slug,
                running,
                stage: stage.as_str().to_string(),
            }
        })
        .collect();
    Ok(statuses)
}
