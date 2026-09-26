// All deploy-time settings are read here and nowhere else.
// Precedence: runtime config (public/config.js) > build-time env (VITE_*) > same origin.

interface RuntimeConfig {
  apiBaseUrl?: string;
}

declare global {
  interface Window {
    __SHADOUCMDB_CONFIG__?: RuntimeConfig;
  }
}

const runtime: RuntimeConfig = (typeof window !== "undefined" && window.__SHADOUCMDB_CONFIG__) || {};

function normaliseBaseUrl(value: string | undefined): string {
  return (value ?? "").trim().replace(/\/+$/, "");
}

export const config = {
  /** Backend base URL without /api/v1. Empty string means "same origin as the UI". */
  apiBaseUrl: normaliseBaseUrl(runtime.apiBaseUrl || import.meta.env.VITE_API_BASE_URL),
};
