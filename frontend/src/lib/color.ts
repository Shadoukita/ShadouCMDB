/**
 * Colour arithmetic for the administrator's brand colours. Only validated
 * "#rrggbb" values reach the page, and only as values of known CSS variables.
 */
export const HEX_COLOR = /^#[0-9a-fA-F]{6}$/;

type Rgb = [number, number, number];

function parse(hex: string): Rgb {
  return [1, 3, 5].map((i) => Number.parseInt(hex.slice(i, i + 2), 16)) as Rgb;
}

function format(rgb: Rgb): string {
  return `#${rgb.map((v) => Math.round(Math.min(255, Math.max(0, v))).toString(16).padStart(2, "0")).join("")}`;
}

/** Mixes `hex` toward `toward` by `amount` (0-1). */
export function mix(hex: string, toward: string, amount: number): string {
  const a = parse(hex);
  const b = parse(toward);
  return format(a.map((v, i) => v + (b[i] - v) * amount) as Rgb);
}

/** WCAG relative luminance. */
function luminance(hex: string): number {
  const [r, g, b] = parse(hex).map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG contrast ratio (1-21). */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** White or near-black, whichever reads better on `bg`. */
export function textOn(bg: string): string {
  return contrast(bg, "#ffffff") >= contrast(bg, "#111111") ? "#ffffff" : "#111111";
}

/** `hex` lightened or darkened in steps until it reaches `ratio` against `bg` (for links and focus rings). */
export function readableOn(hex: string, bg: string, ratio = 4.5): string {
  const toward = luminance(bg) > 0.5 ? "#000000" : "#ffffff";
  let out = hex;
  for (let step = 1; step <= 10 && contrast(out, bg) < ratio; step++) out = mix(hex, toward, step / 10);
  return out;
}
