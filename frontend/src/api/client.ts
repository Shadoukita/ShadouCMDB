// The one API client. Every request the UI makes goes through `api` (typed from
// the backend's OpenAPI spec via schema.d.ts) and `unwrap`, which turns the API's
// error envelope into an ApiError. Components never call fetch directly.
import createClient from "openapi-fetch";
import { config } from "../config";
import type { components, paths } from "./schema";

export type Schemas = components["schemas"];

export const api = createClient<paths>({
  baseUrl: config.apiBaseUrl,
  headers: { "X-Actor-Name": config.actorName },
});

export interface ApiErrorDetail {
  in?: string;
  field: string;
  message: string;
  code?: string;
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly details: ApiErrorDetail[];
  readonly requestId?: string;

  constructor(status: number, code: string, message: string, details: ApiErrorDetail[] = [], requestId?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.details = details;
    this.requestId = requestId;
  }

  /** Field errors keyed by body field path ("name", "attributes.cpu_cores"). Root-level errors use "". */
  fieldErrors(): Record<string, string> {
    const out: Record<string, string> = {};
    for (const d of this.details) {
      const key = d.field === "(root)" ? "" : d.field;
      out[key] = out[key] ? `${out[key]}; ${d.message}` : d.message;
    }
    return out;
  }
}

interface Envelope {
  error?: { code?: string; message?: string; details?: ApiErrorDetail[]; requestId?: string };
}

type FetchResult<T> = { data?: T; error?: unknown; response: Response };

/** Resolves to the response body, or throws ApiError (also for network failures). */
export async function unwrap<T>(request: Promise<FetchResult<T>>): Promise<T> {
  let result: FetchResult<T>;
  try {
    result = await request;
  } catch (cause) {
    throw new ApiError(
      0,
      "NETWORK_ERROR",
      `Cannot reach the ShadouCMDB API at ${config.apiBaseUrl || window.location.origin}. Check that the backend is running and the API base URL is configured.`,
    );
  }
  const { data, error, response } = result;
  if (response.ok) return data as T;
  const env = (error ?? {}) as Envelope;
  if (env.error) {
    throw new ApiError(
      response.status,
      env.error.code ?? "UNKNOWN",
      env.error.message ?? response.statusText,
      env.error.details ?? [],
      env.error.requestId,
    );
  }
  throw new ApiError(
    response.status,
    response.status === 404 ? "NOT_FOUND" : "UNEXPECTED_RESPONSE",
    `The API answered ${response.status} ${response.statusText} without an error body. Is the API base URL pointing at the ShadouCMDB backend?`,
  );
}
