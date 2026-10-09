// Bulk import rules the UI needs, kept free of Vue so they can be unit tested:
// which wizard step a job is at, how statuses read, the browser-side file check,
// and operator-facing wording for the API's refusal codes.
import type { ApiError } from "../api/client";
import type { ImportJob, ImportLimits, ImportStatus } from "../api/imports";
import { formatBytes } from "./format";
import type { IconName } from "../icons/lucide";
import { formatNumber, t, type MessageKey } from "../i18n";

/** A job in one of these states changes by itself on the server, so the UI polls it. */
export const RUNNING: ReadonlySet<ImportStatus> = new Set(["uploading", "queued", "analysing", "validating", "committing"]);
/** A job in one of these states never changes again. */
export const FINAL: ReadonlySet<ImportStatus> = new Set(["completed", "completed_with_errors", "failed", "cancelled", "expired"]);

/**
 * The polling interval for a job (§3.6 and getImport's description): every 1 s for the first 10 s of a phase,
 * then 2 s, then 5 s after a minute. After failed polls it backs off to 30 s; the job keeps running on the server.
 * Nothing is polled in a final or idle state, and TanStack Query pauses the interval while the tab is hidden.
 */
export function pollInterval(job: ImportJob | undefined, failures: number, now = Date.now()): number | false {
  if (!job || !RUNNING.has(job.status)) return false;
  if (failures > 0) return Math.min(30_000, 2_000 * 2 ** (failures - 1));
  const since = Date.parse(job.progress.startedAt ?? job.progress.updatedAt);
  const elapsed = Number.isNaN(since) ? 0 : now - since;
  if (elapsed < 10_000) return 1_000;
  if (elapsed < 60_000) return 2_000;
  return 5_000;
}

export type WizardStep = 1 | 2 | 3 | 4;

/** Step names as catalog keys: render them with `t(s.label)`. */
export const STEPS: { step: WizardStep; label: MessageKey }[] = [
  { step: 1, label: "imports.step.upload" },
  { step: 2, label: "imports.step.map" },
  { step: 3, label: "imports.step.check" },
  { step: 4, label: "imports.step.import" },
];

const PHASE_STEP = { analyse: 1, validate: 3, commit: 4 } as const;

/** The step a job is at, from its status alone, so a reload or a shared link opens the right place (§1.1). */
export function stepOf(job: Pick<ImportJob, "status" | "phase" | "mapping" | "summary">): WizardStep {
  switch (job.status) {
    case "uploading":
    case "analysing":
      return 1;
    case "ready":
      return job.mapping ? 2 : 1;
    case "validating":
    case "validated":
      return 3;
    case "committing":
    case "completed":
    case "completed_with_errors":
      return 4;
    case "queued":
    case "failed":
    case "cancelled":
      return job.phase ? PHASE_STEP[job.phase] : 1;
    case "expired":
      if (job.summary?.committed) return 4;
      if (job.summary) return 3;
      return job.mapping ? 2 : 1;
  }
}

/** The furthest step the user may open: the current one, and step 2 once the file is read (Next on step 1). */
export function lastReachableStep(job: Pick<ImportJob, "status" | "phase" | "mapping" | "summary">): WizardStep {
  const step = stepOf(job);
  return job.status === "ready" ? 2 : step;
}

/** The step to show: the one in the URL if it can be opened, else the job's own. */
export function shownStep(job: Pick<ImportJob, "status" | "phase" | "mapping" | "summary">, requested: unknown): WizardStep {
  const n = Number(requested);
  const last = lastReachableStep(job);
  return Number.isInteger(n) && n >= 1 && n <= last ? (n as WizardStep) : stepOf(job);
}

/** Status wording (a catalog key), with an icon so the badge never relies on colour alone (1.4.1). */
export const STATUS: Record<ImportStatus, { label: MessageKey; icon: IconName; tone: "" | "ok" | "warn" | "danger" | "off" }> = {
  uploading: { label: "imports.status.uploading", icon: "upload", tone: "" },
  queued: { label: "imports.status.queued", icon: "ellipsis", tone: "" },
  analysing: { label: "imports.status.analysing", icon: "ellipsis", tone: "" },
  ready: { label: "imports.status.ready", icon: "circle", tone: "" },
  validating: { label: "imports.status.validating", icon: "ellipsis", tone: "" },
  validated: { label: "imports.status.validated", icon: "circle", tone: "" },
  committing: { label: "imports.status.committing", icon: "ellipsis", tone: "" },
  completed: { label: "imports.status.completed", icon: "circle-check", tone: "ok" },
  completed_with_errors: { label: "imports.status.completed_with_errors", icon: "circle-alert", tone: "warn" },
  failed: { label: "imports.status.failed", icon: "circle-x", tone: "danger" },
  cancelled: { label: "imports.status.cancelled", icon: "minus", tone: "off" },
  expired: { label: "imports.status.expired", icon: "minus", tone: "off" },
};

export type FileFormat = "csv" | "xlsx";

/**
 * The quick check before uploading (extension and size), for a fast, clear message. The server checks again,
 * and decides the format by the file's first bytes.
 */
export function checkFile(file: { name: string; size: number }, limits: ImportLimits): { format: FileFormat } | { error: string } {
  const ext = /\.([^.]+)$/.exec(file.name)?.[1]?.toLowerCase();
  if (ext === "xls" || ext === "xlsm" || ext === "ods") {
    return { error: t("imports.lib.oldFormat", { file: file.name, ext }) };
  }
  if (ext !== "csv" && ext !== "xlsx") {
    return { error: t("imports.lib.notSpreadsheet", { file: file.name }) };
  }
  if (file.size === 0) return { error: t("imports.lib.emptyFile", { file: file.name }) };
  if (file.size > limits.maxFileBytes) {
    return { error: t("imports.lib.fileTooBig", { file: file.name, size: formatBytes(file.size), limit: formatBytes(limits.maxFileBytes) }) };
  }
  return { format: ext };
}

/** The refusal code of an upload: `details[0].code`, else the top-level code. */
export function refusalCode(e: ApiError): string {
  return e.details.find((d) => d.code)?.code ?? e.code;
}

/** What to tell the operator when the server refuses an upload; unknown codes fall back to the API's message. */
export function uploadErrorMessage(e: ApiError, limits: ImportLimits | undefined): string {
  switch (refusalCode(e)) {
    case "import_disabled":
      return `${t("imports.off.title")} ${t("imports.off.hint")}`;
    case "import_busy":
      return t("imports.lib.busy");
    case "import_limit":
      return t("imports.lib.tooManyUnfinished");
    case "import_rate":
      return t("imports.lib.rate");
    case "import_storage_full":
      return t("imports.lib.storageFull");
    case "workbook_encrypted_or_xls":
      return t("imports.lib.encryptedOrXls");
    case "unsupported_format":
      return t("imports.lib.unsupportedFormat");
    case "upload_timeout":
      return t("imports.lib.uploadTimeout");
    case "PAYLOAD_TOO_LARGE":
      return limits ? t("imports.lib.payloadTooLarge", { limit: formatBytes(limits.maxFileBytes) }) : t("imports.lib.payloadTooLargeUnknown");
    default:
      return e.message;
  }
}

/** "Row 12, column C" for a job-level analysis error that points into the file. */
export function errorPlace(error: { row?: number | null; column?: number | null }): string {
  const row = error.row != null ? formatNumber(error.row) : null;
  const column = error.column != null ? columnLetter(error.column) : null;
  if (row !== null && column !== null) return t("imports.lib.placeRowColumn", { row, column });
  if (row !== null) return t("imports.lib.placeRow", { row });
  if (column !== null) return t("imports.lib.placeColumn", { column });
  return "";
}

/** 0 → "A", 25 → "Z", 26 → "AA": columns as spreadsheets name them. */
export function columnLetter(index: number): string {
  let n = index + 1;
  let s = "";
  while (n > 0) {
    const r = (n - 1) % 26;
    s = String.fromCharCode(65 + r) + s;
    n = Math.floor((n - 1) / 26);
  }
  return s;
}

/** Delimiter choices; `label` is a catalog key, rendered with `t(d.label)`. */
export const DELIMITERS: { value: string; label: MessageKey }[] = [
  { value: ",", label: "imports.lib.delimiter.comma" },
  { value: ";", label: "imports.lib.delimiter.semicolon" },
  { value: "\t", label: "imports.lib.delimiter.tab" },
  { value: "|", label: "imports.lib.delimiter.pipe" },
];

/** Encoding choices; `label` is a catalog key, rendered with `t(e.label)`. */
export const ENCODINGS: { value: "utf-8" | "windows-1252" | "iso-8859-1"; label: MessageKey }[] = [
  { value: "utf-8", label: "imports.lib.encoding.utf8" },
  { value: "windows-1252", label: "imports.lib.encoding.windows1252" },
  { value: "iso-8859-1", label: "imports.lib.encoding.latin1" },
];

/** The sheet preselected for a workbook: the one the server read, else the first visible one. */
export function defaultSheet(file: { sheets: string[]; hiddenSheets: string[]; sheet?: string | null }): string | undefined {
  return file.sheet ?? file.sheets.find((s) => !file.hiddenSheets.includes(s)) ?? file.sheets[0];
}
