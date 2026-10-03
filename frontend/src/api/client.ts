// The one API client. Every request the UI makes goes through `api` (typed from
// the backend's OpenAPI spec via schema.d.ts) and `unwrap`, which turns the API's
// error envelope into an ApiError. Components never call fetch directly.
import createClient from "openapi-fetch";
import { config } from "../config";
import { t } from "../i18n/index";
import { csrfFromCookies } from "./csrf";
import type { components, paths } from "./schema";

export type Schemas = components["schemas"];

/** The JSON request body of an operation, e.g. JsonBody<"/api/v1/lookup-lists", "post">. */
export type JsonBody<P extends keyof paths, M extends "post" | "patch" | "put"> = NonNullable<
  (paths[P][M] & { requestBody?: { content: { "application/json": unknown } } })["requestBody"]
>["content"]["application/json"];

/** The query parameters of a GET operation. */
export type ListQuery<P extends keyof paths> = NonNullable<
  (paths[P] & { get: { parameters: { query?: unknown } } })["get"]["parameters"]["query"]
>;

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
  return csrfFromCookies(document.cookie) ?? sessionCsrfToken;
}

/** Paths whose 401 is an answer (wrong password, not signed in yet), not an expired session. */
const AUTH_PATHS = ["/api/v1/auth/login", "/api/v1/auth/login/mfa", "/api/v1/auth/me", "/api/v1/auth/password", "/api/v1/setup"];
let unauthenticatedHandler: (() => void) | undefined;

/** Called when any other request answers 401: the session ended (expired, signed out elsewhere, account disabled). */
export function onSessionEnded(handler: () => void) {
  unauthenticatedHandler = handler;
}

let enrolmentRequiredHandler: (() => void) | undefined;

/** Called when a request answers 403 MFA_ENROLMENT_REQUIRED: a profile the user holds now requires two-factor authentication. */
export function onMfaEnrolmentRequired(handler: () => void) {
  enrolmentRequiredHandler = handler;
}

let emailRequiredHandler: (() => void) | undefined;

/** Called when a request answers 403 EMAIL_REQUIRED: the account has no e-mail yet and must enter one first. */
export function onEmailRequired(handler: () => void) {
  emailRequiredHandler = handler;
}

let reauthenticationRequiredHandler: (() => void) | undefined;

/**
 * Called when a request answers 403 REAUTHENTICATION_REQUIRED: a change to accounts, profiles, API tokens or
 * identity providers needs the password confirmed in the last 10 minutes (GH#498).
 */
export function onReauthenticationRequired(handler: () => void) {
  reauthenticationRequiredHandler = handler;
}

/** GETs the API treats like writes (audited CSV exports, audited import job reads): they need the CSRF token too, so a link on another site cannot run them. */
const CSRF_READS = /\/api\/v1\/((configuration-items\/[^/]+\/impact|business-services\/[^/]+\/members|admin\/config)\/export|imports\/[0-9a-f-]{36}(\/issues|\/error-report)?)$/;

api.use({
  // Every state-changing request echoes the CSRF token; the API rejects it otherwise.
  onRequest({ request }) {
    const token = csrfToken();
    const needsToken = !["GET", "HEAD"].includes(request.method) || CSRF_READS.test(new URL(request.url).pathname);
    if (token && needsToken) request.headers.set("X-CSRF-Token", token);
    requestEpoch.set(request, sessionEpoch);
    return request;
  },
  async onResponse({ request, response }) {
    if (
      response.status === 401 &&
      requestEpoch.get(request) === sessionEpoch &&
      !AUTH_PATHS.some((p) => new URL(request.url).pathname.endsWith(p))
    ) {
      unauthenticatedHandler?.();
    }
    if (response.status === 403 && requestEpoch.get(request) === sessionEpoch) {
      const body = (await response.clone().json().catch(() => null)) as Envelope | null;
      if (body?.error?.code === "MFA_ENROLMENT_REQUIRED") enrolmentRequiredHandler?.();
      if (body?.error?.code === "EMAIL_REQUIRED") emailRequiredHandler?.();
      if (body?.error?.code === "REAUTHENTICATION_REQUIRED") reauthenticationRequiredHandler?.();
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
      t("error.network.body", { url: config.apiBaseUrl || window.location.origin }),
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
    t("error.unexpectedResponse", { status: `${response.status} ${response.statusText}`.trim() }),
  );
}
