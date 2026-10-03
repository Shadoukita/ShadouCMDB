import { readFileSync } from "node:fs";
import { defineConfig, loadEnv, type Plugin } from "vite";
import vue from "@vitejs/plugin-vue";

// The Inter font's OFL-1.1 licence has to ship with the font, so the build puts it in
// dist/assets/ next to the content-hashed woff2 files.
function interLicense(): Plugin {
  return {
    name: "inter-license",
    apply: "build",
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "assets/Inter-LICENSE.txt",
        source: readFileSync(new URL("./src/assets/fonts/inter/LICENSE.txt", import.meta.url)),
      });
    },
  };
}

// The dev server can proxy /api to a backend so the browser stays same-origin
// (no CORS setup needed). The target comes from configuration, never from code.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const proxyTarget = env.API_PROXY_TARGET;
  return {
    plugins: [vue(), interLicense()],
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
