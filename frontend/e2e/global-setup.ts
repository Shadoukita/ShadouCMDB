import { request, type FullConfig } from "@playwright/test";
import { join } from "node:path";

/** Browser and API-request state of the signed-in e2e user (cookies), shared by every test. */
export const STORAGE_STATE = join(import.meta.dirname, ".auth", "state.json");

/**
 * Signs in once before the tests. On a database without users this completes
 * first-run setup as E2E_USERNAME; otherwise E2E_USERNAME / E2E_PASSWORD must
 * be an account holding the Administrator profile (CI creates it with
 * `shadoucmdb create-admin`).
 */
export default async function globalSetup(config: FullConfig) {
  const baseURL = config.projects[0]?.use.baseURL;
  const username = process.env.E2E_USERNAME ?? "e2e-admin";
  const password = process.env.E2E_PASSWORD ?? "e2e-admin-password";
  const ctx = await request.newContext({ baseURL });
  const setup = await ctx.get("/api/v1/setup");
  if (!setup.ok()) throw new Error(`GET /api/v1/setup → ${setup.status()}: is the API running at ${baseURL}?`);
  const res = (await setup.json()).setupRequired
    ? await ctx.post("/api/v1/setup", { data: { username, displayName: "E2E admin", password } })
    : await ctx.post("/api/v1/auth/login", { data: { username, password } });
  if (!res.ok()) throw new Error(`Signing in as ${username} failed: ${res.status()} ${await res.text()} (set E2E_USERNAME / E2E_PASSWORD)`);
  await ctx.storageState({ path: STORAGE_STATE });
  await ctx.dispose();
}
