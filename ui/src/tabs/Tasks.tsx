import { useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  api,
  onTaskEvent,
  type TaskConfig,
  type TaskStage,
  type TaskStatus,
  type WalletEntry,
} from "../lib/ipc";
import {
  cx,
  Field,
  NumberInput,
  PencilIcon,
  PlayIcon,
  PlusIcon,
  StageBadge,
  StopIcon,
  TrashIcon,
} from "../components/ui";

interface LogLine {
  level: string;
  msg: string;
  ts: string;
}

const MAX_LOG_LINES = 200;

function blankTask(walletAddr: string): TaskConfig {
  return {
    opensea_slug: "",
    validator_tip_eth: 0.05,
    max_buys: 3,
    max_price_eth: 0.2,
    target_contract: "",
    deployer: "",
    wallet: walletAddr,
    top_n_targets: 50,
    scrape_coverage_pct: 95,
    collection_supply: 10000,
    reveal_selector: { selector: "", kind: "Typed" },
    hidden_uri: null,
  };
}

export default function TasksTab() {
  const [tasks, setTasks] = useState<TaskConfig[]>([]);
  const [statuses, setStatuses] = useState<Record<string, TaskStatus>>({});
  const [wallets, setWallets] = useState<WalletEntry[]>([]);
  const [logs, setLogs] = useState<Record<string, LogLine[]>>({});
  // Scrape coverage percentage per slug (0 to 100), driven by throttled progress
  // events. Shown next to the badge while a task is scraping.
  const [scrapePct, setScrapePct] = useState<Record<string, number>>({});
  const [openLog, setOpenLog] = useState<string | null>(null);
  const [editing, setEditing] = useState<TaskConfig | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Synchronous guard against double-clicking Start while `start_task` is in
  // flight (a ref, so it takes effect before the next render).
  const busy = useRef(false);

  async function reload() {
    const [t, st, w] = await Promise.all([
      api.listTasks(),
      api.taskStatuses(),
      api.listWallets(),
    ]);
    setTasks(t);
    setStatuses(Object.fromEntries(st.map((s) => [s.slug, s])));
    setWallets(w);
  }

  useEffect(() => {
    reload().catch((e) => setErr(String(e)));
    // StrictMode double-invokes effects in dev, and `listen` is async: a naive
    // `return () => un?.()` can't unlisten the first registration (its promise
    // hasn't resolved when cleanup runs), so every event would be handled
    // twice. The `cancelled` flag unlistens a registration that resolves after
    // cleanup has already run.
    let un: UnlistenFn | undefined;
    let cancelled = false;
    onTaskEvent((ev) => {
      if (ev.type === "stage") {
        setStatuses((prev) => ({
          ...prev,
          [ev.slug]: {
            slug: ev.slug,
            running: ev.stage !== "stopped" && ev.stage !== "idle",
            stage: ev.stage,
          },
        }));
        // Reset the percentage at the start of each scrape so a stale value
        // from a prior reveal cycle doesn't linger.
        if (ev.stage === "scraping") {
          setScrapePct((prev) => ({ ...prev, [ev.slug]: 0 }));
        }
      } else if (ev.type === "progress") {
        const pct =
          ev.total > 0
            ? Math.min(100, Math.round((ev.done / ev.total) * 100))
            : 0;
        setScrapePct((prev) => ({ ...prev, [ev.slug]: pct }));
      } else if (ev.type === "log") {
        setLogs((prev) => {
          const next = [
            ...(prev[ev.slug] ?? []),
            { level: ev.level, msg: ev.msg, ts: ev.ts },
          ];
          return { ...prev, [ev.slug]: next.slice(-MAX_LOG_LINES) };
        });
      }
    }).then((u) => {
      if (cancelled) u();
      else un = u;
    });
    return () => {
      cancelled = true;
      un?.();
    };
  }, []);

  function stageOf(slug: string): TaskStage {
    return statuses[slug]?.stage ?? "idle";
  }
  function runningOf(slug: string): boolean {
    return statuses[slug]?.running ?? false;
  }

  async function start(slug: string) {
    if (busy.current) return;
    busy.current = true;
    setErr(null);
    try {
      await api.startTask(slug);
      setStatuses((p) => ({
        ...p,
        [slug]: { slug, running: true, stage: "monitoring" },
      }));
    } catch (e) {
      setErr(String(e));
    } finally {
      busy.current = false;
    }
  }

  async function stop(slug: string) {
    setErr(null);
    try {
      await api.stopTask(slug);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function remove(slug: string) {
    setErr(null);
    try {
      await api.deleteTask(slug);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  async function saveEditing() {
    if (!editing) return;
    if (!editing.opensea_slug.trim()) return setErr("OpenSea slug is required");
    if (!editing.deployer.trim()) return setErr("Deployer address is required");
    if (!editing.wallet) return setErr("Pick a buying wallet");
    if (!/^(0x)?[0-9a-fA-F]{8}$/.test(editing.reveal_selector.selector.trim()))
      return setErr("Reveal selector must be 4 bytes of hex, e.g. 0x55f804b3");
    setErr(null);
    try {
      await api.saveTask(editing);
      setEditing(null);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="space-y-6">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold">Tasks</h1>
          <p className="text-sm text-muted">
            Tasks are stored in <code className="text-gray-300">task/</code>.
            Start one to monitor its deployer for a reveal.
          </p>
        </div>
        <button
          className="btn-primary"
          onClick={() => {
            setIsNew(true);
            setEditing(blankTask(wallets[0]?.address ?? ""));
          }}
        >
          <PlusIcon /> New task
        </button>
      </header>

      {err && (
        <div className="rounded-lg border border-danger/30 bg-danger/10 px-4 py-2 text-sm text-danger">
          {err}
        </div>
      )}

      {tasks.length === 0 && (
        <div className="card p-10 text-center text-sm text-muted">
          No tasks yet. Create one to get started.
        </div>
      )}

      <div className="space-y-4">
        {tasks.map((t) => {
          const running = runningOf(t.opensea_slug);
          const logLines = logs[t.opensea_slug] ?? [];
          const stage = stageOf(t.opensea_slug);
          const pct = scrapePct[t.opensea_slug];
          return (
            <div key={t.opensea_slug} className="card overflow-hidden">
              <div className="flex items-center gap-4 p-5">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-3">
                    <h3 className="truncate text-base font-bold">
                      {t.opensea_slug}
                    </h3>
                    <StageBadge stage={stage} />
                    {stage === "scraping" && pct != null && (
                      <span className="font-mono text-xs font-semibold text-warn">
                        {pct}%
                      </span>
                    )}
                  </div>
                  <div className="mt-2 flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted">
                    <span>
                      deployer{" "}
                      <span className="font-mono text-gray-300">
                        {t.deployer.slice(0, 10)}…
                      </span>
                    </span>
                    <span>
                      wallet{" "}
                      <span className="font-mono text-gray-300">
                        {t.wallet.slice(0, 8)}…
                      </span>
                    </span>
                    <span>top {t.top_n_targets}</span>
                    <span>
                      max {t.max_buys} buys ≤ {t.max_price_eth} Ξ
                    </span>
                    <span>tip {t.validator_tip_eth} Ξ</span>
                    <span>
                      supply {t.collection_supply} · {t.scrape_coverage_pct}%
                    </span>
                    <span className="text-primary">
                      selector {t.reveal_selector.selector || "—"}
                    </span>
                    {t.hidden_uri && (
                      <span
                        className="font-mono text-amber-400"
                        title={`Ignoring hidden/pre-reveal URI: ${t.hidden_uri}`}
                      >
                        hidden{" "}
                        {t.hidden_uri.length > 24
                          ? `${t.hidden_uri.slice(0, 24)}…`
                          : t.hidden_uri}
                      </span>
                    )}
                  </div>
                </div>

                <div className="flex items-center gap-2">
                  {running ? (
                    <button
                      className="btn-danger"
                      onClick={() => stop(t.opensea_slug)}
                    >
                      <StopIcon /> Stop
                    </button>
                  ) : (
                    <button
                      className="btn-primary"
                      onClick={() => start(t.opensea_slug)}
                      disabled={wallets.length === 0}
                    >
                      <PlayIcon /> Start
                    </button>
                  )}
                  <button
                    className="btn-ghost"
                    onClick={() =>
                      setOpenLog((o) =>
                        o === t.opensea_slug ? null : t.opensea_slug,
                      )
                    }
                  >
                    Logs {logLines.length > 0 && `(${logLines.length})`}
                  </button>
                  <button
                    className="btn-icon"
                    disabled={running}
                    onClick={() => {
                      setIsNew(false);
                      setEditing({ ...t });
                    }}
                    title="Edit"
                  >
                    <PencilIcon />
                  </button>
                  <button
                    className="btn-icon text-danger"
                    disabled={running}
                    onClick={() => remove(t.opensea_slug)}
                    title="Delete"
                  >
                    <TrashIcon />
                  </button>
                </div>
              </div>

              {openLog === t.opensea_slug && (
                <div className="max-h-64 overflow-y-auto border-t border-border bg-bg/60 p-4 font-mono text-xs">
                  {logLines.length === 0 ? (
                    <span className="text-muted">No log lines yet.</span>
                  ) : (
                    logLines.map((l, i) => (
                      <div key={i} className="whitespace-pre-wrap">
                        <span className="text-muted/60">
                          {l.ts.slice(11, 19)}{" "}
                        </span>
                        <span
                          className={cx(
                            l.level === "error" && "text-danger",
                            l.level === "warn" && "text-warn",
                            l.level === "info" && "text-gray-300",
                          )}
                        >
                          {l.msg}
                        </span>
                      </div>
                    ))
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {editing && (
        <TaskEditor
          task={editing}
          isNew={isNew}
          wallets={wallets}
          onChange={setEditing}
          onCancel={() => setEditing(null)}
          onSave={saveEditing}
        />
      )}
    </div>
  );
}

function TaskEditor({
  task,
  isNew,
  wallets,
  onChange,
  onCancel,
  onSave,
}: {
  task: TaskConfig;
  isNew: boolean;
  wallets: WalletEntry[];
  onChange: (t: TaskConfig) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  const set = (patch: Partial<TaskConfig>) => onChange({ ...task, ...patch });
  const setSelector = (patch: Partial<TaskConfig["reveal_selector"]>) =>
    set({ reveal_selector: { ...task.reveal_selector, ...patch } });

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6">
      <div className="card max-h-[90vh] w-full max-w-2xl overflow-y-auto p-6">
        <h2 className="mb-5 text-lg font-bold">
          {isNew ? "New task" : `Edit ${task.opensea_slug}`}
        </h2>

        <div className="grid grid-cols-2 gap-4">
          <Field label="OpenSea slug" hint={isNew ? "= filename" : "locked"}>
            <input
              className="input"
              disabled={!isNew}
              value={task.opensea_slug}
              onChange={(e) => set({ opensea_slug: e.target.value })}
            />
          </Field>
          <Field label="Buying wallet">
            <select
              className="input"
              value={task.wallet}
              onChange={(e) => set({ wallet: e.target.value })}
            >
              <option value="">— select —</option>
              {wallets.map((w) => (
                <option key={w.address} value={w.address}>
                  {w.label || "wallet"} · {w.address.slice(0, 8)}…
                </option>
              ))}
            </select>
          </Field>
          <Field label="Deployer (watched 'from')">
            <input
              className="input font-mono"
              placeholder="0x…"
              value={task.deployer}
              onChange={(e) => set({ deployer: e.target.value })}
            />
          </Field>
          <Field label="Target contract">
            <input
              className="input font-mono"
              placeholder="0x…"
              value={task.target_contract}
              onChange={(e) => set({ target_contract: e.target.value })}
            />
          </Field>
          <Field label="Collection supply">
            <NumberInput
              int
              min={1}
              value={task.collection_supply}
              onChange={(n) => set({ collection_supply: n })}
            />
          </Field>
          <Field label="Scrape coverage %">
            <NumberInput
              int
              min={1}
              max={100}
              value={task.scrape_coverage_pct}
              onChange={(n) => set({ scrape_coverage_pct: n })}
            />
          </Field>
          <Field label="Top N targets">
            <NumberInput
              int
              min={1}
              value={task.top_n_targets}
              onChange={(n) => set({ top_n_targets: n })}
            />
          </Field>
          <Field label="Max buys">
            <NumberInput
              int
              min={1}
              value={task.max_buys}
              onChange={(n) => set({ max_buys: n })}
            />
          </Field>
          <Field label="Max price (Ξ)">
            <NumberInput
              min={0}
              step="0.001"
              value={task.max_price_eth}
              onChange={(n) => set({ max_price_eth: n })}
            />
          </Field>
          <Field label="Validator tip (Ξ)">
            <NumberInput
              min={0}
              step="0.001"
              value={task.validator_tip_eth}
              onChange={(n) => set({ validator_tip_eth: n })}
            />
          </Field>
        </div>

        <div className="mt-5 rounded-lg border border-border bg-surface-2/40 p-4">
          <Field label="Hidden / pre-reveal URI (optional)">
            <input
              className="input font-mono"
              placeholder="ipfs://Qm…  — leave blank if unknown"
              value={task.hidden_uri ?? ""}
              onChange={(e) => set({ hidden_uri: e.target.value || null })}
            />
          </Field>
          <p className="mt-2 text-xs text-muted">
            The collection's placeholder baseURI. A multiConfigure call made for
            any reason re-sends it, so the bot ignores this exact URI and only
            fires when the deployer sets a different one.
          </p>
        </div>

        <div className="mt-5 rounded-lg border border-border bg-surface-2/40 p-4">
          <div className="text-sm font-medium">
            Reveal selector
            <span className="font-normal text-muted">
              {" "}
              — the deployer call that reveals this drop
            </span>
          </div>
          <div className="mt-3 grid grid-cols-2 gap-4">
            <Field label="Selector (4-byte hex)">
              <input
                className="input font-mono"
                placeholder="0x55f804b3"
                value={task.reveal_selector.selector}
                onChange={(e) => setSelector({ selector: e.target.value })}
              />
            </Field>
            <Field label="Decode kind">
              <select
                className="input"
                value={task.reveal_selector.kind}
                onChange={(e) =>
                  setSelector({
                    kind: e.target.value as "Typed" | "MultiConfigure",
                  })
                }
              >
                <option value="Typed">Typed (string arg)</option>
                <option value="MultiConfigure">
                  MultiConfigure (tuple scan)
                </option>
              </select>
            </Field>
          </div>
          <p className="mt-2 text-xs text-muted">
            Common: <code>0x55f804b3</code> setBaseURI · <code>0x30176e13</code>{" "}
            setBaseTokenURI · <code>0x4c261247</code> reveal(string) — all
            Typed. <code>0x911f456b</code> OpenSea/SeaDrop multiConfigure —
            MultiConfigure. Check the contract on Etherscan.
          </p>
        </div>

        <div className="mt-6 flex justify-end gap-3">
          <button className="btn-ghost" onClick={onCancel}>
            Cancel
          </button>
          <button className="btn-primary" onClick={onSave}>
            {isNew ? <PlusIcon /> : null}
            Save task
          </button>
        </div>
      </div>
    </div>
  );
}
