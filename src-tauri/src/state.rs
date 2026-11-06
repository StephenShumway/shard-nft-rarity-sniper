//! Shared application state, `.manage()`d by Tauri and injected into commands.

use std::sync::Arc;

use dashmap::DashMap;
use tauri::async_runtime::JoinHandle;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use rarity_sniper::monitor::MonitorHub;
use rarity_sniper::reporter::UiEventTx;
use rarity_sniper::settings::Settings;
use rarity_sniper::task::TaskStage;
use rarity_sniper::wallet::WalletStore;

/// Everything the commands and background tasks share.
pub struct AppState {
    pub settings: Arc<RwLock<Settings>>,
    pub wallets: Arc<RwLock<WalletStore>>,
    pub hub: Arc<MonitorHub>,
    pub tasks: TaskManager,
    /// Cloned into every [`TaskReporter`](rarity_sniper::reporter::TaskReporter),
    /// drained by `drain_ui_events` in `main.rs` and re-emitted to the webview.
    pub ui_tx: UiEventTx,
    /// Last-known stage per task slug, kept current by the event drain loop so
    /// `task_statuses` can answer a freshly-opened UI.
    pub stages: Arc<DashMap<String, TaskStage>>,
}

/// Tracks the spawned loop + cancellation token for each running task.
#[derive(Default)]
pub struct TaskManager {
    running: DashMap<String, TaskHandle>,
}

struct TaskHandle {
    cancel: CancellationToken,
    /// Held but never awaited. Stopping goes through `cancel`.
    #[allow(dead_code)]
    join: JoinHandle<()>,
}

impl TaskManager {
    #[must_use]
    pub fn is_running(&self, slug: &str) -> bool {
        self.running.contains_key(slug)
    }

    pub fn insert(&self, slug: &str, cancel: CancellationToken, join: JoinHandle<()>) {
        self.running
            .insert(slug.to_string(), TaskHandle { cancel, join });
    }

    /// Cancel a running task (its loop deregisters from the hub + emits
    /// `Stopped` on the way out). Returns whether one was running.
    pub fn stop(&self, slug: &str) -> bool {
        if let Some((_, handle)) = self.running.remove(slug) {
            handle.cancel.cancel();
            true
        } else {
            false
        }
    }
}
