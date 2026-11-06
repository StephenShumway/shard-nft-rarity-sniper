//! Per-task progress reporting: the bridge from the snipe pipeline to the UI
//! and to per-task log files, without coupling the core lib to Tauri.
//!
//! A [`TaskReporter`] is a cheap clonable handle. [`TaskReporter::stage`],
//! [`TaskReporter::info`], [`TaskReporter::warn`], and [`TaskReporter::error`]
//! each do two things: (1) push a serializable [`UiTaskEvent`] onto an
//! unbounded channel that the Tauri layer drains and re-emits to the webview,
//! and (2) append a line to the task's log files. [`TaskReporter::debug`]
//! writes to the debug log only, and [`TaskReporter::progress`] only emits a
//! UI event.
//!
//! Channel sends never block. File writes are tiny synchronous appends
//! serialized by a per-task lock so concurrent writers (e.g. the spawned
//! post-cycle Discord sends) don't interleave lines. High-rate scrape progress
//! is throttled to ≤1 event / 100 ms so it never floods the IPC bridge.
//!
//! Each task writes **two** files under `logs/<slug>/`:
//!   * `<date>.log`: the concise, user-facing log (also streamed to the UI).
//!   * `<date>.debug.log`: a verbose debug record saved to disk only, never
//!     shown in the UI. It is a superset: every user-facing line plus the
//!     fine-grained [`TaskReporter::debug`] lines (detected tx hashes, blocks
//!     seen, decoded URIs, per-buy send details, errors).

use chrono::Utc;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::store::now_millis;
use crate::task::TaskStage;

const PROGRESS_THROTTLE_MS: u64 = 100;

/// Event payload emitted to the frontend (Tauri event `"task-event"`). Tagged by
/// `type` so the UI can switch on it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum UiTaskEvent {
    Stage {
        slug: String,
        stage: TaskStage,
    },
    Log {
        slug: String,
        level: String,
        msg: String,
        ts: String,
    },
    Progress {
        slug: String,
        done: u64,
        total: u64,
    },
}

/// Receiver end drained by the Tauri layer to re-emit events to the webview.
pub type UiEventRx = mpsc::UnboundedReceiver<UiTaskEvent>;
/// Sender shared with every [`TaskReporter`].
pub type UiEventTx = mpsc::UnboundedSender<UiTaskEvent>;

/// Create the UI event channel. The Tauri app keeps the receiver and forwards
/// each event with `app.emit("task-event", ev)`.
#[must_use]
pub fn channel() -> (UiEventTx, UiEventRx) {
    mpsc::unbounded_channel()
}

#[derive(Clone)]
pub struct TaskReporter {
    slug: Arc<str>,
    ui_tx: UiEventTx,
    log_path: Arc<PathBuf>,
    debug_path: Arc<PathBuf>,
    write_lock: Arc<Mutex<()>>,
    last_progress_ms: Arc<AtomicU64>,
}

impl TaskReporter {
    /// Build a reporter for `slug`, creating `logs/<slug>/`. The dated log
    /// files are created on first write, and their date is fixed when the reporter
    /// is built.
    #[must_use]
    pub fn new(slug: &str, ui_tx: UiEventTx) -> Self {
        let dir = Path::new("logs").join(slug);
        let _ = std::fs::create_dir_all(&dir);
        let date = Utc::now().format("%Y-%m-%d");
        let log_path = dir.join(format!("{date}.log"));
        let debug_path = dir.join(format!("{date}.debug.log"));
        Self {
            slug: Arc::from(slug),
            ui_tx,
            log_path: Arc::new(log_path),
            debug_path: Arc::new(debug_path),
            write_lock: Arc::new(Mutex::new(())),
            last_progress_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Announce a stage transition (badge + log line).
    pub fn stage(&self, stage: TaskStage) {
        self.write_line(&format!("STAGE  {}", stage.as_str()));
        let _ = self.ui_tx.send(UiTaskEvent::Stage {
            slug: self.slug.to_string(),
            stage,
        });
    }

    pub fn info(&self, msg: impl Into<String>) {
        self.log("info", msg.into());
    }

    pub fn warn(&self, msg: impl Into<String>) {
        self.log("warn", msg.into());
    }

    pub fn error(&self, msg: impl Into<String>) {
        self.log("error", msg.into());
    }

    /// Write a verbose debug line: saved to `<date>.debug.log` only, never
    /// shown in the UI. Use for the fine-grained pipeline trail (detected tx
    /// hashes, blocks, decoded URIs, per-buy send detail).
    pub fn debug(&self, msg: impl Into<String>) {
        let ts = Utc::now().to_rfc3339();
        self.append(&self.debug_path, &format!("{ts}  DEBUG {}", msg.into()));
    }

    /// Report scrape progress. Throttled to [`PROGRESS_THROTTLE_MS`], but the final
    /// `done == total` update always passes so the bar finishes.
    pub fn progress(&self, done: u64, total: u64) {
        let now = now_millis();
        let last = self.last_progress_ms.load(Ordering::Relaxed);
        let is_final = done >= total;
        if !is_final && now.saturating_sub(last) < PROGRESS_THROTTLE_MS {
            return;
        }
        self.last_progress_ms.store(now, Ordering::Relaxed);
        let _ = self.ui_tx.send(UiTaskEvent::Progress {
            slug: self.slug.to_string(),
            done,
            total,
        });
    }

    fn log(&self, level: &str, msg: String) {
        let ts = Utc::now().to_rfc3339();
        self.write_line(&format!("{ts}  {:<5} {msg}", level.to_uppercase()));
        let _ = self.ui_tx.send(UiTaskEvent::Log {
            slug: self.slug.to_string(),
            level: level.to_string(),
            msg,
            ts,
        });
    }

    /// Write a user-facing line to *both* the user log and the debug log (the
    /// debug log is a complete superset).
    fn write_line(&self, line: &str) {
        let Ok(_guard) = self.write_lock.lock() else {
            return;
        };
        write_to(&self.log_path, line);
        write_to(&self.debug_path, line);
    }

    /// Append a debug-only line to `path` under the write lock.
    fn append(&self, path: &Path, line: &str) {
        let Ok(_guard) = self.write_lock.lock() else {
            return;
        };
        write_to(path, line);
    }
}

/// Synchronous append of one line. Opening per line is fine here: logging is
/// low-rate (stage boundaries, a few info lines per cycle, occasional debug
/// lines. Throttled scrape progress goes to the channel only, never a file).
fn write_to(path: &Path, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}
