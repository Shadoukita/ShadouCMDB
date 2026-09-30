// Bulk import rules the UI needs, kept free of Vue so they can be unit tested:
// which wizard step a job is at, how statuses read, the browser-side file check,
// and operator-facing wording for the API's refusal codes.
import type { ApiError } from "../api/client";
import type { ImportJob, ImportLimits, ImportStatus } from "../api/imports";
import { formatBytes } from "./format";

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

export const STEPS: { step: WizardStep; label: string }[] = [
  { step: 1, label: "Upload" },
  { step: 2, label: "Map columns" },
  { step: 3, label: "Check" },
  { step: 4, label: "Import" },
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

/** Status wording, with a text icon so the badge never relies on colour alone (1.4.1). */
export const STATUS: Record<ImportStatus, { label: string; icon: string; tone: "" | "ok" | "warn" | "danger" | "off" }> = {
  uploading: { label: "Uploading", icon: "↑", tone: "" },
  queued: { label: "Waiting", icon: "…", tone: "" },
  analysing: { label: "Reading the file", icon: "…", tone: "" },
  ready: { label: "Ready to map", icon: "○", tone: "" },
  validating: { label: "Checking", icon: "…", tone: "" },
  validated: { label: "Checked", icon: "○", tone: "" },
  committing: { label: "Importing", icon: "…", tone: "" },
  completed: { label: "Completed", icon: "✓", tone: "ok" },
  completed_with_errors: { label: "Completed with errors", icon: "!", tone: "warn" },
  failed: { label: "Failed", icon: "✕", tone: "danger" },
  cancelled: { label: "Cancelled", icon: "–", tone: "off" },
  expired: { label: "Expired", icon: "–", tone: "off" },
};

export type FileFormat = "csv" | "xlsx";

/**
 * The quick check before uploading (extension and size), for a fast, clear message. The server checks again,
 * and decides the format by the file's first bytes.
 */
export function checkFile(file: { name: string; size: number }, limits: ImportLimits): { format: FileFormat } | { error: string } {
  const ext = /\.([^.]+)$/.exec(file.name)?.[1]?.toLowerCase();
  if (ext === "xls" || ext === "xlsm" || ext === "ods") {
    return { error: `${file.name} is an .${ext} file. Save it as .xlsx (Excel Workbook) or .csv and upload that.` };
  }
  if (ext !== "csv" && ext !== "xlsx") {
    return { error: `${file.name} is not a CSV or XLSX file. Choose a file ending in .csv or .xlsx.` };
  }
  if (file.size === 0) return { error: `${file.name} is empty.` };
  if (file.size > limits.maxFileBytes) {
    return {
      error: `${file.name} is ${formatBytes(file.size)}. The limit is ${formatBytes(limits.maxFileBytes)}. Split the file and import it in parts.`,
    };
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
      return "Bulk import is turned off for this instance. An administrator can turn it on under Administration › Import.";
    case "import_busy":
      return "Another import of yours is still running. Wait until it finishes, or cancel it under Imports, then upload again.";
    case "import_limit":
      return "You have too many unfinished imports. Finish or delete some under Imports, then upload again.";
    case "import_rate":
      return "You have uploaded too many files in the last hour. Try again later.";
    case "import_storage_full":
      return "The server has no room for more uploaded files. Delete finished imports or try again later.";
    case "workbook_encrypted_or_xls":
      return "The workbook is password protected or in the old .xls format. Remove the protection, save it as .xlsx and upload it again.";
    case "unsupported_format":
      return "The file's content does not match its name. Upload a CSV file or an Excel workbook (.xlsx).";
    case "upload_timeout":
      return "The upload took too long and was stopped. Check the connection and try again.";
    case "PAYLOAD_TOO_LARGE":
      return limits
        ? `The file is larger than the limit of ${formatBytes(limits.maxFileBytes)}. Split the file and import it in parts.`
        : "The file is larger than the server accepts. Split the file and import it in parts.";
    default:
      return e.message;
  }
}

/** "Row 12, column C" for a job-level analysis error that points into the file. */
export function errorPlace(error: { row?: number | null; column?: number | null }): string {
  const parts: string[] = [];
  if (error.row != null) parts.push(`row ${error.row.toLocaleString()}`);
  if (error.column != null) parts.push(`column ${columnLetter(error.column)}`);
  const s = parts.join(", ");
  return s ? s[0]!.toUpperCase() + s.slice(1) : "";
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

export const DELIMITERS: { value: string; label: string }[] = [
  { value: ",", label: "Comma ( , )" },
  { value: ";", label: "Semicolon ( ; )" },
  { value: "\t", label: "Tab" },
  { value: "|", label: "Pipe ( | )" },
];

export const ENCODINGS: { value: "utf-8" | "windows-1252" | "iso-8859-1"; label: string }[] = [
  { value: "utf-8", label: "UTF-8" },
  { value: "windows-1252", label: "Windows-1252 (Western European)" },
  { value: "iso-8859-1", label: "ISO-8859-1 (Latin-1)" },
];

/** The sheet preselected for a workbook: the one the server read, else the first visible one. */
export function defaultSheet(file: { sheets: string[]; hiddenSheets: string[]; sheet?: string | null }): string | undefined {
  return file.sheet ?? file.sheets.find((s) => !file.hiddenSheets.includes(s)) ?? file.sheets[0];
}
