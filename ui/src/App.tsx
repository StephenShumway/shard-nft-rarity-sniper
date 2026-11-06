import { useState } from "react";
import SettingsTab from "./tabs/Settings";
import WalletsTab from "./tabs/Wallets";
import TasksTab from "./tabs/Tasks";
import { cx } from "./components/ui";
import { WindowControls, ResizeHandles } from "./components/Titlebar";

type Tab = "tasks" | "wallets" | "settings";

const NAV: Array<{ id: Tab; label: string; icon: string }> = [
  { id: "tasks", label: "Tasks", icon: "◎" },
  { id: "wallets", label: "Wallets", icon: "❖" },
  { id: "settings", label: "Settings", icon: "⚙" },
];

export default function App() {
  const [tab, setTab] = useState<Tab>("tasks");

  return (
    <div className="flex h-screen overflow-hidden">
      <aside className="flex w-56 shrink-0 flex-col border-r border-border bg-surface/60 px-3 pb-5">
        {/* Empty draggable strip that matches the controls bar height on the right
           so the very top edge is a uniform, movable band. */}
        <div data-tauri-drag-region className="h-11 shrink-0" />
        {/* Logo sits level with the page title in the content area, so the two
           read as a single top bar. */}
        <div
          data-tauri-drag-region
          className="mb-8 flex shrink-0 items-center gap-2.5 px-2"
        >
          <img
            src="/appicon2.png"
            alt="Shard"
            className="h-9 w-9 rounded-lg"
            draggable={false}
          />
          <div className="leading-tight">
            <div className="text-sm font-bold tracking-wide">Shard</div>
            <div className="text-[10px] uppercase tracking-widest text-muted">
              NFT Automation
            </div>
          </div>
        </div>

        <nav className="flex flex-col gap-1">
          {NAV.map((n) => (
            <button
              key={n.id}
              onClick={() => setTab(n.id)}
              className={cx(
                "flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium transition",
                tab === n.id
                  ? "bg-primary/20 text-gray-100"
                  : "text-gray-300 hover:bg-surface-2",
              )}
            >
              <span className="w-4 text-center text-base">{n.icon}</span>
              {n.label}
            </button>
          ))}
        </nav>
      </aside>

      <main className="flex min-w-0 flex-1 flex-col">
        {/* Drag strip with the window controls inline at the top-right. */}
        <div
          data-tauri-drag-region
          className="flex h-11 shrink-0 items-center justify-end px-4"
        >
          <WindowControls />
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-none">
          <div className="mx-auto max-w-5xl px-8 pb-8 pt-1">
            {tab === "tasks" && <TasksTab />}
            {tab === "wallets" && <WalletsTab />}
            {tab === "settings" && <SettingsTab />}
          </div>
        </div>
      </main>

      <ResizeHandles />
    </div>
  );
}
