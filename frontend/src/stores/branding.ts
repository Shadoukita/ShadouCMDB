import { defineStore } from "pinia";
import { computed, ref } from "vue";
import { fetchPublicBranding, type PublicBranding, type UiBranding, type UiTheme } from "../api/uiSettings";
import { config } from "../config";
import { HEX_COLOR, mix, readableOn, textOn } from "../lib/color";

export const DEFAULT_APP_NAME = "ShadouCMDB";
const THEME_KEY = "shadoucmdb.theme";

/** What the page applies: the saved branding, or the Customization editor's unsaved draft while it is open. */
export interface EffectiveBranding {
  appName: string;
  primaryColor: string | null;
  accentColor: string | null;
  defaultTheme: UiTheme;
  logoUrl: string | null;
  faviconUrl: string | null;
}

/**
 * App name, colours, theme, logo and favicon. Read from the public
 * GET /ui-settings/branding, so the sign-in page is branded too. The editor
 * sets `preview` to show a draft live; it never leaves the browser until saved.
 */
export const useBrandingStore = defineStore("branding", () => {
  const saved = ref<PublicBranding | null>(null);
  const preview = ref<UiBranding | null>(null);
  /** The operator's own choice in the user menu; null follows the administrator's default. */
  const userTheme = ref<UiTheme | null>(readUserTheme());
  const systemDark = ref(typeof window !== "undefined" && window.matchMedia?.("(prefers-color-scheme: dark)").matches);
  window.matchMedia?.("(prefers-color-scheme: dark)").addEventListener?.("change", (e) => (systemDark.value = e.matches));

  async function load() {
    try {
      saved.value = await fetchPublicBranding();
    } catch {
      // Unreachable API: the app shows its own error; keep the built-in look.
    }
  }

  const effective = computed<EffectiveBranding>(() => {
    const s = saved.value;
    const p = preview.value;
    return {
      appName: (p ? p.appName : s?.appName)?.trim() || DEFAULT_APP_NAME,
      primaryColor: (p ? p.primaryColor : s?.primaryColor) ?? null,
      accentColor: (p ? p.accentColor : s?.accentColor) ?? null,
      defaultTheme: (p ? p.defaultTheme : s?.defaultTheme) ?? "system",
      logoUrl: s?.logo ? assetUrl(s.logo.url) : null,
      faviconUrl: s?.favicon ? assetUrl(s.favicon.url) : null,
    };
  });

  const theme = computed<"light" | "dark">(() => {
    const t = userTheme.value ?? effective.value.defaultTheme;
    return t === "system" ? (systemDark.value ? "dark" : "light") : t;
  });

  function setUserTheme(t: UiTheme | null) {
    userTheme.value = t;
    try {
      if (t) localStorage.setItem(THEME_KEY, t);
      else localStorage.removeItem(THEME_KEY);
    } catch {
      // Storage disabled: the choice lasts for this page load.
    }
  }

  return { saved, preview, userTheme, effective, theme, load, setUserTheme };
});

/** Asset URLs are API paths; the API may live on another origin. */
export function assetUrl(path: string): string {
  return `${config.apiBaseUrl}${path}`;
}

function readUserTheme(): UiTheme | null {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return v === "light" || v === "dark" || v === "system" ? v : null;
  } catch {
    return null;
  }
}

const SURFACE = { light: "#ffffff", dark: "#1a2029" };
const SIDEBAR = { light: "#1e2733", dark: "#0c1016" };
const BRAND_VARS = ["--c-primary", "--c-primary-hover", "--c-primary-text", "--c-link", "--c-focus", "--c-accent"];

/**
 * Applies branding to the document: theme attribute, colour variables, title
 * suffix and favicon. Colours are set only as values of known variables, and
 * only after the #rrggbb check, so no stylesheet text ever comes from settings.
 */
export function applyBranding(b: EffectiveBranding, theme: "light" | "dark") {
  const root = document.documentElement;
  root.dataset.theme = theme;
  for (const v of BRAND_VARS) root.style.removeProperty(v);
  const surface = SURFACE[theme];
  if (b.primaryColor && HEX_COLOR.test(b.primaryColor)) {
    const primary = b.primaryColor;
    root.style.setProperty("--c-primary", primary);
    root.style.setProperty("--c-primary-hover", mix(primary, theme === "dark" ? "#ffffff" : "#000000", 0.15));
    root.style.setProperty("--c-primary-text", textOn(primary));
    root.style.setProperty("--c-link", readableOn(primary, surface));
    root.style.setProperty("--c-focus", readableOn(primary, surface, 3));
  }
  if (b.accentColor && HEX_COLOR.test(b.accentColor)) {
    root.style.setProperty("--c-accent", readableOn(b.accentColor, SIDEBAR[theme], 3));
  }
  let link = document.querySelector<HTMLLinkElement>("link[rel~='icon']");
  if (!link) {
    link = document.createElement("link");
    link.rel = "icon";
    document.head.appendChild(link);
  }
  link.href = b.faviconUrl ?? "data:,";
}
