import qrcode from "qrcode-generator";

/** Modules of white around the code: phone cameras need it to find the code. */
export const QR_QUIET = 4;

export interface QrMatrix {
  /** The dark modules as one SVG path, in module units, quiet zone included. */
  d: string;
  /** The code's width and height in modules, quiet zone included. */
  extent: number;
  /** Whether the module at (row, column), quiet zone excluded, is dark. */
  isDark: (r: number, c: number) => boolean;
  /** Modules per side, quiet zone excluded. */
  count: number;
}

/** A QR code of `value` (error correction M), computed in the browser: nothing goes over the network. */
export function qrMatrix(value: string): QrMatrix {
  const qr = qrcode(0, "M");
  qr.addData(value, "Byte");
  qr.make();
  const n = qr.getModuleCount();
  let d = "";
  for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) if (qr.isDark(r, c)) d += `M${c + QR_QUIET} ${r + QR_QUIET}h1v1h-1z`;
  return { d, extent: n + 2 * QR_QUIET, isDark: (r, c) => qr.isDark(r, c), count: n };
}

/** A label to print or stick on a device: the code with up to two lines of text under it. */
export interface QrLabel {
  value: string;
  /** The first line (the CI's name), in bold. */
  title: string;
  /** The second line (the ident). */
  subtitle?: string;
}

const LABEL_FONT = "Arial, Helvetica, sans-serif";
/** Caption sizes in modules: the text scales with the code. */
const TITLE_SIZE = 2.6;
const SUB_SIZE = 2;
const LINE_GAP = 1;

function captionHeight(l: QrLabel): number {
  return TITLE_SIZE + (l.subtitle ? LINE_GAP + SUB_SIZE : 0) + QR_QUIET;
}

/** Shortens a caption line to about the code's width (the canvas and SVG do not wrap text). */
function fit(text: string, extent: number, size: number): string {
  const max = Math.max(4, Math.floor(extent / (size * 0.55)));
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

const xmlEscape = (s: string) => s.replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`);

/** The label as a standalone SVG document (black on white, any size). */
export function qrLabelSvg(l: QrLabel, modulePx = 8): string {
  const m = qrMatrix(l.value);
  const w = m.extent;
  const h = m.extent + captionHeight(l);
  const title = fit(l.title, w, TITLE_SIZE);
  const sub = l.subtitle ? fit(l.subtitle, w, SUB_SIZE) : "";
  const y1 = m.extent + TITLE_SIZE * 0.8;
  const y2 = y1 + LINE_GAP + SUB_SIZE;
  return [
    `<?xml version="1.0" encoding="UTF-8"?>`,
    `<svg xmlns="http://www.w3.org/2000/svg" width="${w * modulePx}" height="${h * modulePx}" viewBox="0 0 ${w} ${h}" shape-rendering="crispEdges">`,
    `<title>${xmlEscape(l.title)}</title>`,
    `<rect width="100%" height="100%" fill="#fff"/>`,
    `<path d="${m.d}" fill="#000"/>`,
    `<text x="${w / 2}" y="${y1}" text-anchor="middle" font-family="${LABEL_FONT}" font-size="${TITLE_SIZE}" font-weight="bold" fill="#000">${xmlEscape(title)}</text>`,
    sub ? `<text x="${w / 2}" y="${y2}" text-anchor="middle" font-family="${LABEL_FONT}" font-size="${SUB_SIZE}" fill="#000">${xmlEscape(sub)}</text>` : "",
    `</svg>`,
  ].join("");
}

/** The label drawn on a canvas and encoded as PNG, `modulePx` pixels per module (crisp, no scaling). */
export function qrLabelPng(l: QrLabel, modulePx = 10): Promise<Blob> {
  const m = qrMatrix(l.value);
  const w = m.extent;
  const h = m.extent + captionHeight(l);
  const canvas = document.createElement("canvas");
  canvas.width = w * modulePx;
  canvas.height = Math.ceil(h * modulePx);
  const ctx = canvas.getContext("2d");
  if (!ctx) return Promise.reject(new Error("This browser cannot draw the PNG image."));
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = "#000";
  for (let r = 0; r < m.count; r++) for (let c = 0; c < m.count; c++) if (m.isDark(r, c)) ctx.fillRect((c + QR_QUIET) * modulePx, (r + QR_QUIET) * modulePx, modulePx, modulePx);
  ctx.textAlign = "center";
  ctx.font = `bold ${TITLE_SIZE * modulePx}px ${LABEL_FONT}`;
  const y1 = m.extent + TITLE_SIZE * 0.8;
  ctx.fillText(fit(l.title, w, TITLE_SIZE), (w / 2) * modulePx, y1 * modulePx);
  if (l.subtitle) {
    ctx.font = `${SUB_SIZE * modulePx}px ${LABEL_FONT}`;
    ctx.fillText(fit(l.subtitle, w, SUB_SIZE), (w / 2) * modulePx, (y1 + LINE_GAP + SUB_SIZE) * modulePx);
  }
  return new Promise((resolve, reject) => canvas.toBlob((b) => (b ? resolve(b) : reject(new Error("This browser cannot draw the PNG image."))), "image/png"));
}

/** Saves `blob` as `name` through a temporary link (the file is made in the browser). */
export function saveBlob(blob: Blob, name: string) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

/** A file name from the CI's ident (letters, digits, dashes), e.g. `CI-7K3M9Q2X-qr.png`. */
export const qrFileName = (ident: string, ext: "svg" | "png") => `${ident.replace(/[^A-Za-z0-9._-]+/g, "_") || "ci"}-qr.${ext}`;
