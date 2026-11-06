import { getCurrentWindow } from "@tauri-apps/api/window";
import { cx } from "./ui";

// The window is frameless (`decorations: false` in tauri.conf.json), so we draw
// our own window controls and provide the drag + edge-resize affordances the OS
// chrome normally would. The controls are macOS-style "traffic lights" placed
// inline at the top-right of the window. Each window call needs a matching
// `core:window:allow-*` permission in src-tauri/capabilities/default.json.
const appWindow = getCurrentWindow();

// `ResizeDirection` isn't exported from `@tauri-apps/api/window`, so recover it
// from the method signature. It's a string union ("North" | "South" | …) that
// the backend receives verbatim, so the literals below are what it expects.
type ResizeDir = Parameters<typeof appWindow.startResizeDragging>[0];

// Three macOS-style dots. Glyphs are hidden until the group is hovered, matching
// the platform behaviour where all three reveal their symbols together.
const DOTS = [
  {
    key: "close",
    title: "Close",
    fill: "#ff5f57",
    border: "#e0443e",
    onClick: () => appWindow.close(),
    glyph: (
      <path
        d="M3.4 3.4 L8.6 8.6 M8.6 3.4 L3.4 8.6"
        stroke="#5e0000"
        strokeWidth="1.9"
        strokeLinecap="round"
      />
    ),
  },
  {
    key: "minimize",
    title: "Minimize",
    fill: "#febc2e",
    border: "#dea123",
    onClick: () => appWindow.minimize(),
    glyph: (
      <path d="M3.2 6 H8.8" stroke="#6b4500" strokeWidth="2.1" strokeLinecap="round" />
    ),
  },
  {
    key: "maximize",
    title: "Maximize",
    fill: "#28c840",
    border: "#13a626",
    onClick: () => appWindow.toggleMaximize(),
    // Two opposed triangles = the macOS fullscreen/zoom mark. Filled (not
    // stroked) so it reads boldly at this size.
    glyph: (
      <g fill="#0a4f12">
        <path d="M3.3 3.3 H7 L3.3 7 Z" />
        <path d="M8.7 8.7 H5 L8.7 5 Z" />
      </g>
    ),
  },
];

/**
 * macOS-style window controls. Meant to be dropped into a `data-tauri-drag-region`
 * header so the surrounding area drags the window while the dots stay clickable.
 */
export function WindowControls() {
  return (
    <div className="group flex items-center gap-2">
      {DOTS.map((d) => (
        <button
          key={d.key}
          type="button"
          title={d.title}
          aria-label={d.title}
          onClick={d.onClick}
          style={{ backgroundColor: d.fill, borderColor: d.border }}
          className="flex h-3 w-3 items-center justify-center rounded-full border transition active:brightness-90"
        >
          <svg
            viewBox="0 0 12 12"
            className="h-[9px] w-[9px] opacity-0 transition-opacity duration-100 group-hover:opacity-100"
            aria-hidden
          >
            {d.glyph}
          </svg>
        </button>
      ))}
    </div>
  );
}

// Thin invisible strips at each edge/corner. A frameless window loses the OS
// resize grips, so each strip forwards a mouse-down to `startResizeDragging`.
// Corners sit above edges (higher z) so diagonal resize wins at the corners.
const HANDLES: Array<{ dir: ResizeDir; cls: string }> = [
  { dir: "North" as ResizeDir, cls: "top-0 inset-x-0 h-1 cursor-ns-resize z-[100]" },
  { dir: "South" as ResizeDir, cls: "bottom-0 inset-x-0 h-1 cursor-ns-resize z-[100]" },
  { dir: "West" as ResizeDir, cls: "left-0 inset-y-0 w-1 cursor-ew-resize z-[100]" },
  { dir: "East" as ResizeDir, cls: "right-0 inset-y-0 w-1 cursor-ew-resize z-[100]" },
  { dir: "NorthWest" as ResizeDir, cls: "top-0 left-0 h-2.5 w-2.5 cursor-nwse-resize z-[101]" },
  { dir: "NorthEast" as ResizeDir, cls: "top-0 right-0 h-2.5 w-2.5 cursor-nesw-resize z-[101]" },
  { dir: "SouthWest" as ResizeDir, cls: "bottom-0 left-0 h-2.5 w-2.5 cursor-nesw-resize z-[101]" },
  { dir: "SouthEast" as ResizeDir, cls: "bottom-0 right-0 h-2.5 w-2.5 cursor-nwse-resize z-[101]" },
];

export function ResizeHandles() {
  return (
    <>
      {HANDLES.map((h) => (
        <div
          key={h.dir}
          onMouseDown={(e) => {
            e.preventDefault();
            void appWindow.startResizeDragging(h.dir);
          }}
          className={cx("fixed", h.cls)}
        />
      ))}
    </>
  );
}
