import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Vite root is this `ui/` folder. Tauri loads the dev server at port 5173 and
// bundles the production build from `ui/dist`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
  },
});
