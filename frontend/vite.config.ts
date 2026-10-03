import { readFileSync } from "node:fs";
import { defineConfig, loadEnv, type Plugin } from "vite";
import vue from "@vitejs/plugin-vue";

// The fonts' OFL-1.1 licences and the Lucide icons' ISC licence have to ship with them, so the
// build puts them in dist/assets/ next to the content-hashed files.
const FONT_LICENSES = {
  "assets/Inter-LICENSE.txt": "./src/assets/fonts/inter/LICENSE.txt",
  "assets/JetBrainsMono-LICENSE.txt": "./src/assets/fonts/jetbrains-mono/LICENSE.txt",
  "assets/Lucide-LICENSE.txt": "./src/icons/LICENSE.txt",
};

function fontLicenses(): Plugin {
  return {
    name: "font-licenses",
    apply: "build",
    generateBundle() {
      for (const [fileName, path] of Object.entries(FONT_LICENSES)) {
        this.emitFile({ type: "asset", fileName, source: readFileSync(new URL(path, import.meta.url)) });
      }
    },
  };
}

// The dev server can proxy /api to a backend so the browser stays same-origin
// (no CORS setup needed). The target comes from configuration, never from code.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const proxyTarget = env.API_PROXY_TARGET;
  return {
    plugins: [vue(), fontLicenses()],
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
