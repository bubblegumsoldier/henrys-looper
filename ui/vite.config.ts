import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// There is no backend to proxy to any more: the engine lives in the same process as this window
// and is reached through Tauri's invoke(). `tauri dev` expects this exact port, so it is fixed.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // Tauri ships its own window; a source map is worth more than a small bundle here.
    sourcemap: true,
  },
});
