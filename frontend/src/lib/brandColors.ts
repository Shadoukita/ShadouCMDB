/**
 * The CSS variables an administrator's brand colours set (Customization › Branding).
 * Pure: stores/branding.ts applies the result to <html>. Every derived colour is checked
 * against the surface it is drawn on, so a brand colour can never take text or focus
 * rings below WCAG AA.
 */
import { HEX_COLOR, mix, readableOn, textOn } from "./color";

export type Theme = "light" | "dark";

/** --c-surface and --c-sidebar of styles/tokens.css (unit/brandColors.test.ts keeps them in step). */
export const SURFACE: Record<Theme, string> = { light: "#ffffff", dark: "#151922" };
export const SIDEBAR: Record<Theme, string> = { light: "#11151c", dark: "#0a0c11" };
/** --c-primary and --c-accent of styles/tokens.css: what an unset brand colour looks like (the colour pickers start there). */
export const DEFAULT_PRIMARY: Record<Theme, string> = { light: "#0a6c7d", dark: "#4cc3d9" };
export const DEFAULT_ACCENT: Record<Theme, string> = { light: "#4cc3d9", dark: "#4cc3d9" };

/** Every variable brandVariables can set, so a cleared colour falls back to tokens.css. */
export const BRAND_VARS = [
  "--c-primary",
  "--c-primary-hover",
  "--c-primary-text",
  "--c-primary-subtle",
  "--c-primary-muted",
  "--c-link",
  "--c-focus",
  "--c-accent",
] as const;

export function brandVariables(primaryColor: string | null, accentColor: string | null, theme: Theme): Record<string, string> {
  const out: Record<string, string> = {};
  const surface = SURFACE[theme];
  if (primaryColor && HEX_COLOR.test(primaryColor)) {
    const text = textOn(primaryColor);
    out["--c-primary"] = primaryColor;
    // Hover moves away from the text colour, so the label never loses contrast on hover.
    out["--c-primary-hover"] = mix(primaryColor, text === "#ffffff" ? "#000000" : "#ffffff", 0.15);
    out["--c-primary-text"] = text;
    // Tints for selected rows and active filters. Links are checked against the tint, the
    // weaker of the two backgrounds they sit on, so they keep AA on the surface too.
    const subtle = mix(primaryColor, surface, theme === "dark" ? 0.82 : 0.92);
    out["--c-primary-subtle"] = subtle;
    out["--c-primary-muted"] = mix(primaryColor, surface, theme === "dark" ? 0.72 : 0.84);
    out["--c-link"] = readableOn(primaryColor, subtle);
    out["--c-focus"] = readableOn(primaryColor, surface, 3);
  }
  if (accentColor && HEX_COLOR.test(accentColor)) {
    // The accent is drawn on the dark rail only (tokens.css).
    out["--c-accent"] = readableOn(accentColor, SIDEBAR[theme], 3);
  }
  return out;
}
