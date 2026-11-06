/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      // Cool-slate palette (11-step ramp, light → dark):
      //   F0F1F4 E2E4E9 C4C9D4 A7AEBE 8A93A8 6C7893 576075 414858 2B303B 16181D 0F1115
      // Dark steps drive the bg→surface→border ramp. Light steps drive text,
      // muted, and the primary accent. `bg` and `surface` are interpolated
      // between the 2B303B and 16181D steps. `surface` is a gentle lift so cards
      // separate from the background without needing harsh borders.
      colors: {
        bg: "#181B21",
        surface: "#1F232B",
        "surface-2": "#2B303B",
        border: "#414858",
        // Mid-tone blue-slate accent (palette step #6C7893). Dark enough that
        // primary buttons carry white text. `text-primary` accents stay legible
        // thanks to their tinted backgrounds.
        primary: {
          DEFAULT: "#6C7893",
          hover: "#7C89A6",
          dim: "#576075",
        },
        ok: "#3ddc97",
        warn: "#f5a623",
        danger: "#ff5c6c",
        muted: "#8A93A8",
        // Deep-merges into Tailwind's gray scale. Only the text shades the app
        // actually uses are remapped onto the palette's light end.
        gray: {
          100: "#F0F1F4",
          200: "#C4C9D4",
          300: "#A7AEBE",
        },
      },
      fontFamily: {
        sans: ["Inter", "Segoe UI", "system-ui", "sans-serif"],
        mono: ["JetBrains Mono", "Consolas", "monospace"],
      },
      boxShadow: {
        // Neutral elevation: faint light-slate ring (palette step C4C9D4) + a
        // soft dark drop.
        glow: "0 0 0 1px rgba(196,201,212,0.22), 0 10px 24px -10px rgba(0,0,0,0.7)",
      },
    },
  },
  plugins: [],
};
