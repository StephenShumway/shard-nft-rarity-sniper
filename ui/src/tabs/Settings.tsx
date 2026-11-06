import { useEffect, useState } from "react";
import { api, type Settings } from "../lib/ipc";
import {
  cx,
  Field,
  NumberInput,
  PlayIcon,
  PlusIcon,
  Section,
  Spinner,
  TrashIcon,
} from "../components/ui";

type TestState = { ok: boolean; ms: number; detail: string } | null;

function TestButton({ run }: { run: () => Promise<{ ok: boolean; ms: number; detail: string }> }) {
  const [busy, setBusy] = useState(false);
  const [res, setRes] = useState<TestState>(null);

  async function go() {
    setBusy(true);
    setRes(null);
    try {
      setRes(await run());
    } catch (e) {
      setRes({ ok: false, ms: 0, detail: String(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex items-center gap-2">
      <button type="button" className="btn-icon" onClick={go} disabled={busy} title="Test">
        {busy ? <Spinner /> : <PlayIcon className="text-primary" />}
      </button>
      {res && (
        <span className={cx("text-xs font-medium", res.ok ? "text-ok" : "text-danger")}>
          {res.ok ? (res.ms > 0 ? `${res.ms} ms` : "ok") : "failed"}
          <span className="ml-1 font-normal text-muted">· {res.detail}</span>
        </span>
      )}
    </div>
  );
}

export default function SettingsTab() {
  const [s, setS] = useState<Settings | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    api.getSettings().then(setS).catch((e) => setErr(String(e)));
  }, []);

  if (!s) {
    return <div className="text-sm text-muted">Loading settings…</div>;
  }

  const update = (patch: Partial<Settings>) => setS({ ...s, ...patch });

  async function save() {
    if (!s) return;
    setSaving(true);
    setErr(null);
    try {
      await api.saveSettings(s);
      setSavedAt(Date.now());
    } catch (e) {
      setErr(String(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-6">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold">Settings</h1>
          <p className="text-sm text-muted">
            Global config — written to <code className="text-gray-300">settings/settings.json</code>.
          </p>
        </div>
        <div className="flex items-center gap-3">
          {savedAt && <span className="text-xs text-ok">Saved</span>}
          <button className="btn-primary" onClick={save} disabled={saving}>
            {saving ? <Spinner /> : null}
            Save settings
          </button>
        </div>
      </header>

      {err && (
        <div className="rounded-lg border border-danger/30 bg-danger/10 px-4 py-2 text-sm text-danger">
          {err}
        </div>
      )}

      <Section title="RPC & OpenSea" subtitle="Ethereum endpoint and OpenSea API access.">
        <div className="space-y-4">
          <Field label="Ethereum RPC (HTTPS)">
            <div className="flex items-center gap-3">
              <input
                className="input font-mono"
                value={s.eth_rpc_url}
                placeholder="https://…"
                onChange={(e) => update({ eth_rpc_url: e.target.value })}
              />
              <TestButton
                run={async () => {
                  const r = await api.testRpc(s.eth_rpc_url);
                  return { ok: r.ok, ms: r.ms, detail: r.detail };
                }}
              />
            </div>
          </Field>
          <div className="grid grid-cols-2 gap-4">
            <Field label="OpenSea API key">
              <input
                className="input font-mono"
                value={s.opensea_api_key}
                onChange={(e) => update({ opensea_api_key: e.target.value })}
              />
            </Field>
            <Field label="IPFS concurrency" hint="per gateway">
              <NumberInput
                int
                min={1}
                value={s.ipfs_concurrency}
                onChange={(n) => update({ ipfs_concurrency: n })}
              />
            </Field>
          </div>
        </div>
      </Section>

      <Section title="bloxroute (mempool)" subtitle="Pending-tx WebSocket stream.">
        <div className="space-y-4">
          <Field label="Stream URL (wss)">
            <div className="flex items-center gap-3">
              <input
                className="input font-mono"
                value={s.blox_stream_url}
                placeholder="wss://…blxrbdn.com/ws"
                onChange={(e) => update({ blox_stream_url: e.target.value })}
              />
              <TestButton run={() => api.testEndpoint(s.blox_stream_url)} />
            </div>
          </Field>
          <Field label="Authorization header">
            <input
              className="input font-mono"
              value={s.blox_auth_header}
              onChange={(e) => update({ blox_auth_header: e.target.value })}
            />
          </Field>
        </div>
      </Section>

      <Section title="QuickNode webhook" subtitle="Confirmed-block backstop receiver.">
        <div className="grid grid-cols-2 gap-4">
          <Field label="Webhook port">
            <NumberInput
              int
              min={1}
              max={65535}
              value={s.stream_port}
              onChange={(n) => update({ stream_port: n })}
            />
          </Field>
          <Field label="Security token (HMAC)" hint="optional">
            <input
              className="input font-mono"
              value={s.qn_security_token ?? ""}
              onChange={(e) => update({ qn_security_token: e.target.value || null })}
            />
          </Field>
        </div>
      </Section>

      <Section
        title="Discord webhook"
        subtitle="Posts a notification after each confirmed snipe. Off the hot path."
      >
        <Field label="Webhook URL" hint="optional">
          <div className="flex items-center gap-3">
            <input
              className="input font-mono"
              placeholder="https://discord.com/api/webhooks/…"
              value={s.discord_webhook ?? ""}
              onChange={(e) => update({ discord_webhook: e.target.value || null })}
            />
            <TestButton
              run={async () => {
                await api.testDiscord(s.discord_webhook ?? "");
                return { ok: true, ms: 0, detail: "test sent" };
              }}
            />
          </div>
        </Field>
      </Section>

      <Section
        title="Pinata / IPFS gateways"
        subtitle="Round-robined by the scraper. Key is optional per gateway."
        actions={
          <button
            className="btn-ghost"
            onClick={() => update({ gateways: [...s.gateways, { url: "", key: "" }] })}
          >
            <PlusIcon /> Add gateway
          </button>
        }
      >
        <div className="space-y-3">
          {s.gateways.length === 0 && (
            <p className="text-sm text-muted">No gateways yet — add at least one.</p>
          )}
          {s.gateways.map((g, i) => (
            <div key={i} className="flex items-center gap-3">
              <input
                className="input font-mono"
                placeholder="https://gw.mypinata.cloud/ipfs/"
                value={g.url}
                onChange={(e) => {
                  const gateways = [...s.gateways];
                  gateways[i] = { ...g, url: e.target.value };
                  update({ gateways });
                }}
              />
              <input
                className="input w-48 font-mono"
                placeholder="gateway token"
                value={g.key ?? ""}
                onChange={(e) => {
                  const gateways = [...s.gateways];
                  gateways[i] = { ...g, key: e.target.value || null };
                  update({ gateways });
                }}
              />
              <TestButton run={() => api.testEndpoint(g.url)} />
              <button
                className="btn-icon text-danger"
                onClick={() => update({ gateways: s.gateways.filter((_, j) => j !== i) })}
              >
                <TrashIcon />
              </button>
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Builder relays"
        subtitle="eth_sendBundle targets. Each signed snipe is broadcast to all."
        actions={
          <button className="btn-ghost" onClick={() => update({ relays: [...s.relays, ""] })}>
            <PlusIcon /> Add relay
          </button>
        }
      >
        <div className="space-y-3">
          {s.relays.length === 0 && (
            <p className="text-sm text-muted">No relays yet — add Titan/Quasar/BuilderNet etc.</p>
          )}
          {s.relays.map((r, i) => (
            <div key={i} className="flex items-center gap-3">
              <input
                className="input font-mono"
                placeholder="https://rpc.titanbuilder.xyz"
                value={r}
                onChange={(e) => {
                  const relays = [...s.relays];
                  relays[i] = e.target.value;
                  update({ relays });
                }}
              />
              <TestButton run={() => api.testEndpoint(r)} />
              <button
                className="btn-icon text-danger"
                onClick={() => update({ relays: s.relays.filter((_, j) => j !== i) })}
              >
                <TrashIcon />
              </button>
            </div>
          ))}
        </div>
      </Section>
    </div>
  );
}
