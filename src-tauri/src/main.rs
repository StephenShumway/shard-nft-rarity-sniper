// Hide the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;
mod tasks;

use std::sync::Arc;

use dashmap::DashMap;
use tauri::Emitter;
use tokio::sync::RwLock;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use rarity_sniper::monitor::MonitorHub;
use rarity_sniper::reporter::{self, UiEventRx, UiTaskEvent};
use rarity_sniper::settings::Settings;
use rarity_sniper::task::TaskStage;
use rarity_sniper::wallet::WalletStore;

use state::{AppState, TaskManager};

fn main() {
    let _log_guard = init_tracing();

    let settings = Arc::new(RwLock::new(Settings::load()));
    let wallets = Arc::new(RwLock::new(WalletStore::load()));
    let hub = Arc::new(MonitorHub::new(Arc::clone(&settings)));
    let (ui_tx, ui_rx) = reporter::channel();
    let stages: Arc<DashMap<String, TaskStage>> = Arc::new(DashMap::new());

    let app_state = AppState {
        settings,
        wallets,
        hub,
        tasks: TaskManager::default(),
        ui_tx,
        stages: Arc::clone(&stages),
    };

    tauri::Builder::default()
        .manage(app_state)
        .setup(move |app| {
            let handle = app.handle().clone();
            // Drain per-task reporter events → update the stage cache + emit to UI.
            tauri::async_runtime::spawn(drain_ui_events(handle, ui_rx, stages));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::test_endpoint,
            commands::test_rpc,
            commands::test_discord,
            commands::list_wallets,
            commands::add_wallet,
            commands::remove_wallet,
            commands::fetch_balance,
            commands::list_tasks,
            commands::save_task,
            commands::delete_task,
            commands::start_task,
            commands::stop_task,
            commands::task_statuses,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Forward every [`UiTaskEvent`] to the webview as a `"task-event"` and keep the
/// per-task stage cache current for `task_statuses`.
async fn drain_ui_events(
    app: tauri::AppHandle,
    mut rx: UiEventRx,
    stages: Arc<DashMap<String, TaskStage>>,
) {
    while let Some(ev) = rx.recv().await {
        if let UiTaskEvent::Stage { slug, stage } = &ev {
            stages.insert(slug.clone(), *stage);
        }
        let _ = app.emit("task-event", &ev);
    }
}

/// Log to stdout and a daily-rotated `logs/desktop.log.<date>` (relative to the
/// working directory). The returned guard must live for the process lifetime
/// or buffered log lines are dropped.
fn init_tracing() -> WorkerGuard {
    let file_appender = tracing_appender::rolling::daily("logs", "desktop.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_ansi(false),
        )
        .init();

    guard
}
