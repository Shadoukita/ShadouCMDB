import { request, type FullConfig } from "@playwright/test";
import { join } from "node:path";

/** Browser and API-request state of the signed-in e2e user (cookies), shared by every test. */
export const STORAGE_STATE = join(import.meta.dirname, ".auth", "state.json");

/** The administrator the tests run as (created by first-run setup, or given by CI). */
export const E2E_USER = {
  username: process.env.E2E_USERNAME ?? "e2e-admin",
  password: process.env.E2E_PASSWORD ?? "e2e-admin-password",
};

/**
 * The fresh installs of e2e/fresh-install.spec.ts (E2E_BARE_BASE_URL, E2E_IMPORT_BASE_URL): first-run
 * setup creates this administrator on each, and their sessions are stored next to STORAGE_STATE.
 */
export const FRESH_ADMIN = { username: "fresh-admin", displayName: "Fresh Administrator", password: "fresh-install-password-123" };
export const BARE_STATE = join(import.meta.dirname, ".auth", "bare.json");
export const IMPORT_TARGET_STATE = join(import.meta.dirname, ".auth", "import-target.json");
/** The fresh installs of e2e/area-tables.spec.ts (E2E_AREAS_BASE_URL, E2E_AREAS_IMPORT_BASE_URL), also set up as FRESH_ADMIN. */
export const AREAS_STATE = join(import.meta.dirname, ".auth", "areas.json");
export const AREAS_IMPORT_STATE = join(import.meta.dirname, ".auth", "areas-import.json");

/** Completes first-run setup on `baseURL` when no user exists yet, otherwise signs in; saves the cookies to `path`. */
async function signIn(baseURL: string | undefined, user: { username: string; password: string; displayName: string }, path: string) {
  const ctx = await request.newContext({ baseURL });
  const setup = await ctx.get("/api/v1/setup");
  if (!setup.ok()) throw new Error(`GET /api/v1/setup → ${setup.status()}: is the API running at ${baseURL}?`);
  const res = (await setup.json()).setupRequired
    ? await ctx.post("/api/v1/setup", { data: user })
    : await ctx.post("/api/v1/auth/login", { data: { username: user.username, password: user.password } });
  if (!res.ok()) throw new Error(`Signing in as ${user.username} at ${baseURL} failed: ${res.status()} ${await res.text()}`);
  await ctx.storageState({ path });
  await ctx.dispose();
}

/**
 * Signs in once before the tests. On a database without users this completes
 * first-run setup as E2E_USERNAME; otherwise E2E_USERNAME / E2E_PASSWORD must
 * be an account holding the Administrator profile (CI creates it with
 * `shadoucmdb create-admin`).
 */
export default async function globalSetup(config: FullConfig) {
  const baseURL = config.projects[0]?.use.baseURL;
  await signIn(baseURL, { ...E2E_USER, displayName: "E2E admin" }, STORAGE_STATE).catch((e: Error) => {
    throw new Error(`${e.message} (set E2E_USERNAME / E2E_PASSWORD)`);
  });
  if (process.env.E2E_BARE_BASE_URL) await signIn(process.env.E2E_BARE_BASE_URL, FRESH_ADMIN, BARE_STATE);
  if (process.env.E2E_IMPORT_BASE_URL) await signIn(process.env.E2E_IMPORT_BASE_URL, FRESH_ADMIN, IMPORT_TARGET_STATE);
  if (process.env.E2E_AREAS_BASE_URL) await signIn(process.env.E2E_AREAS_BASE_URL, FRESH_ADMIN, AREAS_STATE);
  if (process.env.E2E_AREAS_IMPORT_BASE_URL) await signIn(process.env.E2E_AREAS_IMPORT_BASE_URL, FRESH_ADMIN, AREAS_IMPORT_STATE);
}
