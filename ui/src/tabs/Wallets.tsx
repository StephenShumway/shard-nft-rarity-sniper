import { useEffect, useState } from "react";
import { api, type WalletEntry } from "../lib/ipc";
import {
  EyeIcon,
  EyeOffIcon,
  Field,
  PlusIcon,
  RefreshIcon,
  Section,
  Spinner,
  TrashIcon,
} from "../components/ui";

function shortAddr(a: string) {
  return a.length > 12 ? `${a.slice(0, 6)}…${a.slice(-4)}` : a;
}

function fmtTime(secs?: number | null) {
  if (!secs) return "never";
  return new Date(secs * 1000).toLocaleString();
}

function WalletRow({
  w,
  onRemove,
  onBalance,
}: {
  w: WalletEntry;
  onRemove: () => void;
  onBalance: () => Promise<void>;
}) {
  const [reveal, setReveal] = useState(false);
  const [busy, setBusy] = useState(false);

  return (
    <div className="flex items-center gap-4 rounded-lg border border-border bg-surface-2/50 px-4 py-3">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="font-semibold">{w.label || "Wallet"}</span>
          <span className="font-mono text-xs text-muted">{shortAddr(w.address)}</span>
        </div>
        <div className="mt-1 flex items-center gap-2">
          <code className="truncate text-xs text-muted">
            {reveal ? w.private_key : "•".repeat(24)}
          </code>
          <button className="text-muted hover:text-gray-200" onClick={() => setReveal((v) => !v)}>
            {reveal ? <EyeOffIcon /> : <EyeIcon />}
          </button>
        </div>
      </div>

      <div className="text-right">
        <div className="font-mono text-sm font-semibold">
          {w.balance_eth != null ? `${w.balance_eth.toFixed(4)} Ξ` : "—"}
        </div>
        <div className="text-[10px] text-muted">{fmtTime(w.balance_updated_at)}</div>
      </div>

      <button
        className="btn-ghost"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            await onBalance();
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy ? <Spinner /> : <RefreshIcon />}
        Balance
      </button>
      <button className="btn-icon text-danger" onClick={onRemove} title="Remove">
        <TrashIcon />
      </button>
    </div>
  );
}

export default function WalletsTab() {
  const [wallets, setWallets] = useState<WalletEntry[]>([]);
  const [pk, setPk] = useState("");
  const [label, setLabel] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);

  const reload = () => api.listWallets().then(setWallets).catch((e) => setErr(String(e)));
  useEffect(() => {
    reload();
  }, []);

  async function add() {
    setErr(null);
    setAdding(true);
    try {
      await api.addWallet(pk.trim(), label.trim() || undefined);
      setPk("");
      setLabel("");
      await reload();
    } catch (e) {
      setErr(String(e));
    } finally {
      setAdding(false);
    }
  }

  async function remove(address: string) {
    await api.removeWallet(address);
    await reload();
  }

  async function balance(address: string) {
    setErr(null);
    try {
      await api.fetchBalance(address);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-xl font-bold">Wallets</h1>
        <p className="text-sm text-muted">
          Stored in <code className="text-gray-300">wallet/wallet.json</code>. Keys are plain text —
          keep this machine secure.
        </p>
      </header>

      {err && (
        <div className="rounded-lg border border-danger/30 bg-danger/10 px-4 py-2 text-sm text-danger">
          {err}
        </div>
      )}

      <Section title="Import wallet" subtitle="Address is derived from the private key.">
        <div className="flex items-end gap-3">
          <div className="flex-1">
            <Field label="Private key">
              <input
                className="input font-mono"
                placeholder="0x…"
                value={pk}
                onChange={(e) => setPk(e.target.value)}
              />
            </Field>
          </div>
          <div className="w-48">
            <Field label="Label" hint="optional">
              <input
                className="input"
                placeholder="main"
                value={label}
                onChange={(e) => setLabel(e.target.value)}
              />
            </Field>
          </div>
          <button className="btn-primary" disabled={!pk.trim() || adding} onClick={add}>
            {adding ? <Spinner /> : <PlusIcon />} Import
          </button>
        </div>
      </Section>

      <Section title={`Imported wallets (${wallets.length})`}>
        <div className="space-y-3">
          {wallets.length === 0 && <p className="text-sm text-muted">No wallets imported yet.</p>}
          {wallets.map((w) => (
            <WalletRow
              key={w.address}
              w={w}
              onRemove={() => remove(w.address)}
              onBalance={() => balance(w.address)}
            />
          ))}
        </div>
      </Section>
    </div>
  );
}
