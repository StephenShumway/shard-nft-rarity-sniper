import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ── Types mirroring the Rust structs ────────────────────────────────────────

export interface Gateway {
  url: string;
  key?: string | null;
}

export interface Settings {
  eth_rpc_url: string;
  blox_stream_url: string;
  blox_auth_header: string;
  gateways: Gateway[];
  opensea_api_key: string;
  ipfs_concurrency: number;
  relays: string[];
  stream_port: number;
  qn_security_token?: string | null;
  discord_webhook?: string | null;
  /** Deployed snipe contract, kept backend-only: `get_settings` always returns
   * "" and `save_settings` ignores this field, so the UI can't read or change it. */
  sniper_contract: string;
}

export interface WalletEntry {
  label: string;
  address: string;
  private_key: string;
  balance_eth?: number | null;
  balance_updated_at?: number | null;
}

export type DecodeKind = "Typed" | "MultiConfigure";

/** The deployer function call that counts as a task's reveal. */
export interface RevealSelector {
  /** 4-byte selector as hex, e.g. "0x55f804b3" for setBaseURI(string). */
  selector: string;
  kind: DecodeKind;
}

export interface TaskConfig {
  opensea_slug: string;
  validator_tip_eth: number;
  max_buys: number;
  max_price_eth: number;
  target_contract: string;
  deployer: string;
  wallet: string;
  top_n_targets: number;
  scrape_coverage_pct: number;
  collection_supply: number;
  reveal_selector: RevealSelector;
  /** Known pre-reveal/placeholder baseURI to ignore so a non-reveal
   * multiConfigure (which re-sends the hidden URI) doesn't fire a snipe. */
  hidden_uri?: string | null;
}

export interface PingResult {
  ok: boolean;
  ms: number;
  detail: string;
}

export interface RpcResult {
  ok: boolean;
  ms: number;
  block: number;
  detail: string;
}

export type TaskStage =
  | "idle"
  | "monitoring"
  | "scraping"
  | "buying"
  | "success"
  | "failed"
  | "stopped";

export interface TaskStatus {
  slug: string;
  running: boolean;
  stage: TaskStage;
}

export type TaskEvent =
  | { type: "stage"; slug: string; stage: TaskStage }
  | { type: "log"; slug: string; level: string; msg: string; ts: string }
  | { type: "progress"; slug: string; done: number; total: number };

// ── Command wrappers (camelCase args → snake_case Rust params) ───────────────

export const api = {
  // Settings
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  testEndpoint: (url: string) => invoke<PingResult>("test_endpoint", { url }),
  testRpc: (url: string) => invoke<RpcResult>("test_rpc", { url }),
  testDiscord: (url: string) => invoke<void>("test_discord", { url }),

  // Wallets
  listWallets: () => invoke<WalletEntry[]>("list_wallets"),
  addWallet: (privateKey: string, label?: string) =>
    invoke<WalletEntry>("add_wallet", { privateKey, label }),
  removeWallet: (address: string) => invoke<void>("remove_wallet", { address }),
  fetchBalance: (address: string) => invoke<number>("fetch_balance", { address }),

  // Tasks
  listTasks: () => invoke<TaskConfig[]>("list_tasks"),
  saveTask: (task: TaskConfig) => invoke<void>("save_task", { task }),
  deleteTask: (slug: string) => invoke<void>("delete_task", { slug }),
  startTask: (slug: string) => invoke<void>("start_task", { slug }),
  stopTask: (slug: string) => invoke<void>("stop_task", { slug }),
  taskStatuses: () => invoke<TaskStatus[]>("task_statuses"),
};

/** Subscribe to backend task events. Returns an unlisten function. */
export function onTaskEvent(cb: (ev: TaskEvent) => void): Promise<UnlistenFn> {
  return listen<TaskEvent>("task-event", (e) => cb(e.payload));
}
