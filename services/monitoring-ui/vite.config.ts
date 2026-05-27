import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api/monitoring": {
        target: process.env.MONITORING_API_URL ?? "http://127.0.0.1:8000",
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/api\/monitoring/, "/admin/monitoring"),
        headers: process.env.MONITORING_OPERATOR_API_KEY
          ? { "x-api-key": process.env.MONITORING_OPERATOR_API_KEY }
          : undefined,
      },
    },
  },
});
