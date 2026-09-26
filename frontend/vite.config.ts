import { defineConfig, loadEnv } from "vite";
import vue from "@vitejs/plugin-vue";

// The dev server can proxy /api to a backend so the browser stays same-origin
// (no CORS setup needed). The target comes from configuration, never from code.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const proxyTarget = env.API_PROXY_TARGET;
  return {
    plugins: [vue()],
    server: {
      port: Number(env.WEB_PORT ?? 5173),
      proxy: proxyTarget ? { "/api": { target: proxyTarget, changeOrigin: true } } : undefined,
    },
    preview: {
      port: Number(env.WEB_PORT ?? 4173),
      proxy: proxyTarget ? { "/api": { target: proxyTarget, changeOrigin: true } } : undefined,
    },
  };
});
