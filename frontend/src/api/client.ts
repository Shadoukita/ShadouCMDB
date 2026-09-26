// The one API client. Every request the UI makes goes through `api` (typed from
// the backend's OpenAPI spec via schema.d.ts) and `unwrap`, which turns the API's
// error envelope into an ApiError. Components never call fetch directly.
import createClient from "openapi-fetch";
import { config } from "../config";
import type { components, paths } from "./schema";

export type Schemas = components["schemas"];

export const api = createClient<paths>({
  baseUrl: config.apiBaseUrl,
  // The session cookie also travels when the API is on another origin (CORS_ORIGINS on the server).
  credentials: "include",
});

/** The CSRF token from the last sign-in or /auth/me (needed when the API is on another origin and its cookie is unreadable). */
let sessionCsrfToken: string | undefined;

/** Bumped at every sign-in and sign-out, so a late answer to a request from the previous session is not mistaken for this one's. */
let sessionEpoch = 0;
const requestEpoch = new WeakMap<Request, number>();

export function setCsrfToken(token: string | undefined) {
  sessionCsrfToken = token;
  sessionEpoch++;
}

/** The session's CSRF token: the readable cookie the API sets at sign-in, else the one from the session. */
function csrfToken(): string | undefined {
  const match = document.cookie.match(/(?:^|;\s*)shadoucmdb_csrf=([^;]+)/);
  return match?.[1] ?? sessionCsrfToken;
}

/** Paths whose 401 is an answer (wrong password, not signed in yet), not an expired session. */
const AUTH_PATHS = ["/api/v1/auth/login", "/api/v1/auth/me", "/api/v1/auth/password", "/api/v1/setup"];
let unauthenticatedHandler: (() => void) | undefined;

/** Called when any other request answers 401: the session ended (expired, signed out elsewhere, account disabled). */
export function onSessionEnded(handler: () => void) {
  unauthenticatedHandler = handler;
}

api.use({
  // Every state-changing request echoes the CSRF token; the API rejects it otherwise.
  onRequest({ request }) {
    const token = csrfToken();
    if (token && !["GET", "HEAD"].includes(request.method)) request.headers.set("X-CSRF-Token", token);
    requestEpoch.set(request, sessionEpoch);
    return request;
  },
  onResponse({ request, response }) {
    if (
      response.status === 401 &&
      requestEpoch.get(request) === sessionEpoch &&
      !AUTH_PATHS.some((p) => new URL(request.url).pathname.endsWith(p))
    ) {
      unauthenticatedHandler?.();
    }
    return response;
  },
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
