import type { APIRequestContext, APIResponse } from "@playwright/test";
import { apiGet, apiSend, csrf, expect, test } from "./support";

// Release regressions without other e2e coverage (SHAA-463):
// - the legacy statuses, environments, locations and owners are read-only: writes answer 410 GONE (GH#111);
// - identity providers cannot be added, changed or deleted with an API token, reading still works (GitHub #137);
// - two administrators resetting, at once, users who created tokens for each other both succeed (GH#166).
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "release-regressions-password-1";
const SESSION_ONLY = "This endpoint needs a signed-in session; API tokens cannot call it";

type Ctx = { request: { newContext: (o: object) => Promise<APIRequestContext> } };
interface Session {
  ctx: APIRequestContext;
  send: (method: string, path: string, data?: unknown) => Promise<APIResponse>;
}

const users: string[] = [];
let adminProfileId = "";

async function signIn(playwright: Ctx, baseURL: string, username: string): Promise<Session> {
  const ctx = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const res = await ctx.post("/api/v1/auth/login", { data: { username, password: PASSWORD } });
  expect(res.status(), `sign in as ${username}: ${await res.text()}`).toBe(200);
  const token = (await res.json()).csrfToken as string;
  return { ctx, send: (method, path, data) => ctx.fetch(`/api/v1${path}`, { method, data, headers: { "X-CSRF-Token": token } }) };
}

async function createUser(request: APIRequestContext, name: string, profileIds: string[]): Promise<{ id: string; username: string }> {
  const user = await apiSend<{ id: string; username: string }>(request, "POST", "/admin/users", {
    username: `e2e-relreg-${name}-${stamp}`,
    email: `e2e-relreg-${name}-${stamp}@example.test`,
    displayName: `E2E release regressions ${name} ${stamp}`,
    password: PASSWORD,
    profileIds,
  });
  users.push(user.id);
  return user;
}

async function mint(who: Session, owner: string, name: string): Promise<string> {
  const expiresAt = new Date(Date.now() + 7 * 86_400_000).toISOString();
  const res = await who.send("POST", "/admin/api-tokens", { name: `E2E ${name} ${stamp}`, userId: owner, profileId: adminProfileId, expiresAt });
  expect(res.status(), `mint ${name}: ${await res.text()}`).toBe(201);
  return ((await res.json()) as { secret: string }).secret;
}

test.beforeAll(async ({ request }) => {
  const list = await apiGet<{ data: { id: string; name: string }[] }>(request, "/admin/profiles?limit=200");
  adminProfileId = list.data.find((p) => p.name === "Administrator")!.id;
});

test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of users.splice(0)) expect((await request.delete(`/api/v1/admin/users/${id}`, { headers })).status()).toBe(200);
});

test("legacy statuses, environments, locations and owners: reads work, writes answer 410 GONE and change nothing (GH#111)", async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const kind of ["statuses", "environments", "locations", "owners"]) {
    const list = await request.get(`/api/v1/${kind}`);
    expect(list.status(), kind).toBe(200);
    const row = ((await list.json()) as { data: { id: string }[] }).data[0];
    const id = row?.id ?? "00000000-0000-4000-8000-000000000001";
    const before = row ? await (await request.get(`/api/v1/${kind}/${id}`)).json() : null;

    const writes = [
      request.post(`/api/v1/${kind}`, { headers, data: { key: `e2e_${stamp}`, name: `E2E ${stamp}` } }),
      request.patch(`/api/v1/${kind}/${id}`, { headers, data: { name: `E2E changed ${stamp}` } }),
      request.delete(`/api/v1/${kind}/${id}`, { headers }),
    ];
    for (const res of await Promise.all(writes)) {
      expect(res.status(), `${res.url()} → ${res.status()}`).toBe(410);
      const { error } = await res.json();
      expect(error.code).toBe("GONE");
      expect(error.message).toContain("/api/v1/lookup-list-values");
    }
    if (before) expect(await (await request.get(`/api/v1/${kind}/${id}`)).json()).toEqual(before);
  }
});

test("identity providers refuse API tokens for add, change, delete and the connection test; reading accepts them (GitHub #137, #192)", async ({ request, playwright, baseURL }) => {
  const admin = await createUser(request, "idp-admin", [adminProfileId]);
  const session = await signIn(playwright, baseURL!, admin.username);
  const created = await session.send("POST", "/admin/identity-providers", {
    kind: "oidc",
    name: `E2E session-only ${stamp}`,
    oidc: { issuerUrl: "https://127.0.0.1:9/", clientId: "e2e" },
  });
  expect(created.status(), await created.text()).toBe(201);
  const idp = (await created.json()) as { id: string; name: string };
  const secret = await mint(session, admin.id, "idp");
  const token = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] }, extraHTTPHeaders: { Authorization: `Bearer ${secret}` } });

  const writes = [
    token.post("/api/v1/admin/identity-providers", { data: { kind: "oidc", name: `E2E by token ${stamp}`, oidc: { issuerUrl: "https://127.0.0.1:9/", clientId: "x" } } }),
    token.patch(`/api/v1/admin/identity-providers/${idp.id}`, { data: { isEnabled: false } }),
    token.delete(`/api/v1/admin/identity-providers/${idp.id}`),
    token.post(`/api/v1/admin/identity-providers/${idp.id}/test`, { data: {} }),
  ];
  for (const res of await Promise.all(writes)) {
    expect(res.status(), `${res.url()} → ${res.status()}`).toBe(403);
    expect((await res.json()).error).toMatchObject({ code: "FORBIDDEN", message: SESSION_ONLY });
  }
  expect((await token.get("/api/v1/admin/identity-providers")).status()).toBe(200);
  const read = await token.get(`/api/v1/admin/identity-providers/${idp.id}`);
  expect(read.status()).toBe(200);
  expect(await read.json()).toMatchObject({ name: idp.name, isEnabled: true });

  expect((await session.send("DELETE", `/admin/identity-providers/${idp.id}`)).status()).toBe(204);
  await token.dispose();
  await session.ctx.dispose();
});

test("two administrators resetting, at once, users who created tokens for each other both succeed (GH#166)", async ({ request, playwright, baseURL }) => {
  test.slow(); // several sign-ins, each an argon2id check
  const second = await signIn(playwright, baseURL!, (await createUser(request, "resetter", [adminProfileId])).username);
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (let round = 0; round < 3; round++) {
    const x = await createUser(request, `x${round}`, [adminProfileId]);
    const y = await createUser(request, `y${round}`, [adminProfileId]);
    const xs = await signIn(playwright, baseURL!, x.username);
    const ys = await signIn(playwright, baseURL!, y.username);
    await mint(xs, y.id, `x${round} for y`);
    await mint(ys, x.id, `y${round} for x`);
    const [a, b] = await Promise.all([
      request.put(`/api/v1/admin/users/${x.id}/password`, { headers, data: { password: `${PASSWORD}-reset` } }),
      second.send("PUT", `/admin/users/${y.id}/password`, { password: `${PASSWORD}-reset` }),
    ]);
    expect([a.status(), b.status()], `round ${round}: ${await a.text()} ${await b.text()}`).toEqual([200, 200]);
    await xs.ctx.dispose();
    await ys.ctx.dispose();
  }
  await second.ctx.dispose();
});
