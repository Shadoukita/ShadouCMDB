// Saving a file the API sends as an attachment (the CSV exports). The request goes through the typed
// client (it needs the session's CSRF token, which a plain link cannot send), so the file is read into a
// Blob and handed to the browser as a download.
import { ApiError, unwrap } from "./client";
import { t } from "../i18n";

type BlobResult = { data?: unknown; error?: unknown; response: Response };

/**
 * Runs `request` (a GET with `parseAs: "blob"`) and saves the file under the name the server gives, or
 * `fallbackName`. Throws ApiError for an error response, and for a download cut short.
 */
export async function saveDownload(request: () => Promise<BlobResult>, fallbackName: string): Promise<void> {
  let result: BlobResult;
  try {
    result = await request();
  } catch (cause) {
    if (cause instanceof ApiError) throw cause;
    // A streamed file that breaks off (the server ends a failed export with a broken connection) rejects
    // while its body is read; a request that never got an answer rejects the same way.
    throw new ApiError(0, "DOWNLOAD_INTERRUPTED", t("download.interrupted"));
  }
  const { data, error, response } = result;
  if (!response.ok) await unwrap(Promise.resolve({ data: undefined, error, response }));
  // The header is unreadable when the API is on another origin and does not expose it.
  const name = /filename="?([^";]+)"?/.exec(response.headers.get("Content-Disposition") ?? "")?.[1] ?? fallbackName;
  const a = document.createElement("a");
  a.href = URL.createObjectURL(data as Blob);
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

/** A local timestamp for a file name: 20261008-1432. */
export function fileStamp(now = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}`;
}
