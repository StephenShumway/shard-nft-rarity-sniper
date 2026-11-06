import { useEffect, useState, type ReactNode, type SVGProps } from "react";
import type { TaskStage } from "../lib/ipc";

export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

// ── Numeric input ───────────────────────────────────────────────────────────

/**
 * Number field backed by a string draft.
 *
 * A plain `value={n}` + `onChange={Number(x) || 0}` binding can't be cleared
 * (empty → 0), and once stuck at 0 every keystroke reads as "0" + digit. This
 * keeps the raw string while focused, so the field can be empty mid-edit, and
 * commits a parsed number on each parseable change. `min`/`max` clamp and `int`
 * rounds every committed value. The visible draft is normalized on blur.
 */
export function NumberInput({
  value,
  onChange,
  min,
  max,
  int = false,
  step,
  className,
  placeholder,
}: {
  value: number;
  onChange: (n: number) => void;
  min?: number;
  max?: number;
  int?: boolean;
  step?: number | string;
  className?: string;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState<string>(String(value));
  const [focused, setFocused] = useState(false);

  // While unfocused, mirror external value changes (e.g. form reset / load).
  useEffect(() => {
    if (!focused) setDraft(String(value));
  }, [value, focused]);

  function clamp(n: number): number {
    let v = int ? Math.round(n) : n;
    if (min != null && v < min) v = min;
    if (max != null && v > max) v = max;
    return v;
  }

  return (
    <input
      type="text"
      inputMode={int ? "numeric" : "decimal"}
      className={cx("input", className)}
      placeholder={placeholder}
      value={draft}
      step={step}
      onFocus={() => setFocused(true)}
      onChange={(e) => {
        const raw = e.target.value;
        // Allow empty, lone "-", "0.", "1." etc. while typing.
        if (/^-?\d*\.?\d*$/.test(raw)) {
          setDraft(raw);
          const n = Number(raw);
          if (raw !== "" && raw !== "-" && raw !== "." && Number.isFinite(n)) {
            onChange(clamp(n));
          }
        }
      }}
      onBlur={() => {
        setFocused(false);
        const n = Number(draft);
        const normalized = draft === "" || !Number.isFinite(n) ? (min ?? 0) : clamp(n);
        onChange(normalized);
        setDraft(String(normalized));
      }}
    />
  );
}

// ── Icons (inline, currentColor) ────────────────────────────────────────────

function Icon(props: SVGProps<SVGSVGElement> & { children: ReactNode }) {
  const { children, ...rest } = props;
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      width={16}
      height={16}
      {...rest}
    >
      {children}
    </svg>
  );
}

export const PlayIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <polygon points="6 4 20 12 6 20 6 4" fill="currentColor" stroke="none" />
  </Icon>
);
export const StopIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" stroke="none" />
  </Icon>
);
export const TrashIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6" />
  </Icon>
);
export const PlusIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <path d="M12 5v14M5 12h14" />
  </Icon>
);
export const RefreshIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6" />
  </Icon>
);
export const EyeIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
    <circle cx="12" cy="12" r="3" />
  </Icon>
);
export const EyeOffIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20C5 20 1 12 1 12a18.5 18.5 0 0 1 5.06-5.94M9.9 4.24A9 9 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19M1 1l22 22" />
  </Icon>
);
export const PencilIcon = (p: SVGProps<SVGSVGElement>) => (
  <Icon {...p}>
    <path d="M12 20h9M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4z" />
  </Icon>
);

// ── Spinner ─────────────────────────────────────────────────────────────────

export function Spinner({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={16}
      height={16}
      className={cx("animate-spin", className)}
    >
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="3" fill="none" opacity="0.25" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" strokeWidth="3" fill="none" strokeLinecap="round" />
    </svg>
  );
}

// ── Stage badge ─────────────────────────────────────────────────────────────

const STAGE_STYLES: Record<TaskStage, { label: string; cls: string }> = {
  idle: { label: "Idle", cls: "bg-surface-2 text-muted border-border" },
  monitoring: { label: "Monitoring", cls: "bg-sky-500/15 text-sky-300 border-sky-500/30" },
  scraping: { label: "Scraping", cls: "bg-warn/15 text-warn border-warn/30" },
  buying: { label: "Buying", cls: "bg-primary/15 text-primary border-primary/40" },
  success: { label: "Success", cls: "bg-ok/15 text-ok border-ok/30" },
  failed: { label: "Failed", cls: "bg-danger/15 text-danger border-danger/30" },
  stopped: { label: "Stopped", cls: "bg-surface-2 text-muted border-border" },
};

export function StageBadge({ stage }: { stage: TaskStage }) {
  const s = STAGE_STYLES[stage] ?? STAGE_STYLES.idle;
  const pulse = stage === "monitoring" || stage === "scraping" || stage === "buying";
  return (
    <span
      className={cx(
        "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-semibold",
        s.cls,
      )}
    >
      <span className={cx("h-1.5 w-1.5 rounded-full bg-current", pulse && "animate-pulse")} />
      {s.label}
    </span>
  );
}

// ── Section card ────────────────────────────────────────────────────────────

export function Section({
  title,
  subtitle,
  children,
  actions,
}: {
  title: string;
  subtitle?: string;
  children: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <section className="card p-5">
      <div className="mb-4 flex items-start justify-between gap-4">
        <div>
          <h2 className="text-sm font-semibold tracking-wide text-gray-100">{title}</h2>
          {subtitle && <p className="mt-0.5 text-xs text-muted">{subtitle}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

// ── Labeled text field ──────────────────────────────────────────────────────

export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label className="block">
      <span className="label">
        {label}
        {hint && <span className="ml-2 font-normal text-muted/70">{hint}</span>}
      </span>
      {children}
    </label>
  );
}
