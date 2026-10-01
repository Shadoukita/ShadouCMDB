import type { APIRequestContext, APIResponse } from "@playwright/test";
import { apiGet, apiSend, classIdByName, csrf, expect, test } from "./support";

// Token security and the /usage gate (SHAA-416):
// - an administrator's password reset revokes the user's API tokens and the ones they created for others (GH#145);
//   their own password change revokes only their own;
// - permission profiles and configuration import need a signed-in session, API tokens get 403 (GH#154);
// - /usage needs datamodel.manage, so it cannot count what a user may not see (GH#121).
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "token-security-password-1";
const SESSION_ONLY = "This endpoint needs a signed-in session; API tokens cannot call it";

type Ctx = { request: { newContext: (o: object) => Promise<APIRequestContext> } };
interface Session {
  ctx: APIRequestContext;
  send: (method: string, path: string, data?: unknown) => Promise<APIResponse>;
}
interface Token {
  id: string;
  secret: string;
}

const users: string[] = [];
const profiles: string[] = [];
let adminProfileId = "";

async function signIn(playwright: Ctx, baseURL: string, username: string, password = PASSWORD): Promise<Session> {
  const ctx = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const res = await ctx.post("/api/v1/auth/login", { data: { username, password } });
  expect(res.status(), `sign in as ${username}: ${await res.text()}`).toBe(200);
  const token = (await res.json()).csrfToken as string;
  return { ctx, send: (method, path, data) => ctx.fetch(`/api/v1${path}`, { method, data, headers: { "X-CSRF-Token": token } }) };
}

/** A context authenticated only by the bearer token. */
async function bearer(playwright: Ctx, baseURL: string, secret: string) {
  return playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] }, extraHTTPHeaders: { Authorization: `Bearer ${secret}` } });
}

async function works(playwright: Ctx, baseURL: string, token: Token): Promise<number> {
  const ctx = await bearer(playwright, baseURL, token.secret);
  const status = (await ctx.get("/api/v1/configuration-items?limit=1")).status();
  await ctx.dispose();
  return status;
}

async function createUser(request: APIRequestContext, name: string, profileIds: string[]): Promise<{ id: string; username: string }> {
  const user = await apiSend<{ id: string; username: string }>(request, "POST", "/admin/users", {
    username: `e2e-toksec-${name}-${stamp}`,
    displayName: `E2E token security ${name} ${stamp}`,
    password: PASSWORD,
    profileIds,
  });
  users.push(user.id);
  return user;
}

async function mint(who: Session, owner: string, name: string): Promise<Token> {
  const expiresAt = new Date(Date.now() + 7 * 86_400_000).toISOString();
  const res = await who.send("POST", "/admin/api-tokens", { name: `E2E ${name} ${stamp}`, userId: owner, profileId: adminProfileId, expiresAt });
  expect(res.status(), `mint ${name}: ${await res.text()}`).toBe(201);
  const body = (await res.json()) as { token: { id: string }; secret: string };
  return { id: body.token.id, secret: body.secret };
}

test.beforeAll(async ({ request }) => {
  const list = await apiGet<{ data: { id: string; name: string }[] }>(request, "/admin/profiles?limit=200");
  adminProfileId = list.data.find((p) => p.name === "Administrator")!.id;
});

// Deleted users take their tokens with them; the Users list stays as short as the other specs expect.
test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of users.splice(0)) expect((await request.delete(`/api/v1/admin/users/${id}`, { headers })).status()).toBe(200);
  for (const id of profiles.splice(0)) expect((await request.delete(`/api/v1/admin/profiles/${id}`, { headers })).status()).toBe(204);
});

test("an administrator's password reset revokes the user's tokens and the tokens they created for others (GH#145)", async ({ request, playwright, baseURL }) => {
  const alice = await createUser(request, "alice", [adminProfileId]);
  const bob = await createUser(request, "bob", [adminProfileId]);
  const carol = await createUser(request, "carol", [adminProfileId]);
  const asAlice = await signIn(playwright, baseURL!, alice.username);
  const asBob = await signIn(playwright, baseURL!, bob.username);
  const aliceOwn = await mint(asAlice, alice.id, "alice own");
  const aliceForBob = await mint(asAlice, bob.id, "alice for bob");
  const aliceForCarol = await mint(asAlice, carol.id, "alice for carol");
  const bobOwn = await mint(asBob, bob.id, "bob own");
  const bobForAlice = await mint(asBob, alice.id, "bob for alice");
  for (const t of [aliceOwn, aliceForBob, aliceForCarol, bobOwn, bobForAlice]) expect(await works(playwright, baseURL!, t)).toBe(200);

  // The createdBy filter finds what alice minted.
  const minted = await apiGet<{ data: { id: string; createdByUserId: string | null }[] }>(request, `/admin/api-tokens?createdBy=${alice.id}&limit=50`);
  expect(minted.data.map((t) => t.id).sort()).toEqual([aliceOwn.id, aliceForBob.id, aliceForCarol.id].sort());

  const reset = await request.put(`/api/v1/admin/users/${alice.id}/password`, { data: { password: `${PASSWORD}-reset` }, headers: { "X-CSRF-Token": await csrf(request) } });
  expect(reset.status(), await reset.text()).toBe(200);

  // Everything alice owns or created is refused; bob's own token is not hers and keeps working.
  for (const t of [aliceOwn, bobForAlice, aliceForBob, aliceForCarol]) expect(await works(playwright, baseURL!, t), `token ${t.id}`).toBe(401);
  expect(await works(playwright, baseURL!, bobOwn)).toBe(200);
  expect((await asAlice.send("GET", "/configuration-items?limit=1")).status()).toBe(401);

  const after = await apiGet<{ data: { status: string; revokedBy: string | null }[] }>(request, `/admin/api-tokens?createdBy=${alice.id}&limit=50`);
  expect(after.data).toHaveLength(3);
  for (const t of after.data) expect(t).toMatchObject({ status: "revoked", revokedBy: expect.any(String) });
  // Each revocation is in the audit log.
  const audit = await apiGet<{ data: { entityId: string; action: string }[] }>(request, "/audit-log?entityType=api_tokens&limit=200");
  const updated = new Set(audit.data.filter((e) => e.action === "update").map((e) => e.entityId));
  for (const t of [aliceOwn, bobForAlice, aliceForBob, aliceForCarol]) expect(updated.has(t.id), `audit row for ${t.id}`).toBe(true);

  await asAlice.ctx.dispose();
  await asBob.ctx.dispose();
});

test("a user's own password change revokes their own tokens, not the ones they created for others", async ({ request, playwright, baseURL }) => {
  const dave = await createUser(request, "dave", [adminProfileId]);
  const erin = await createUser(request, "erin", [adminProfileId]);
  const asDave = await signIn(playwright, baseURL!, dave.username);
  const daveOwn = await mint(asDave, dave.id, "dave own");
  const daveForErin = await mint(asDave, erin.id, "dave for erin");

  const change = await asDave.send("PUT", "/auth/password", { currentPassword: PASSWORD, newPassword: `${PASSWORD}-changed` });
  expect(change.status(), await change.text()).toBe(204);
  expect(await works(playwright, baseURL!, daveOwn)).toBe(401);
  expect(await works(playwright, baseURL!, daveForErin)).toBe(200);
  // The session that changed the password stays signed in.
  expect((await asDave.send("GET", "/configuration-items?limit=1")).status()).toBe(200);
  await asDave.ctx.dispose();
});

test("permission profiles and configuration import refuse API tokens, reading still accepts them (GH#154)", async ({ request, playwright, baseURL }) => {
  const target = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: `E2E token target ${stamp}`, globalPermissions: [] });
  profiles.push(target.id);
  const before = await apiGet<unknown>(request, `/admin/profiles/${target.id}`);
  // A token of the signed-in e2e administrator, with the Administrator profile.
  const created = await apiSend<{ token: { id: string }; secret: string }>(request, "POST", "/admin/api-tokens", {
    name: `E2E profile writer ${stamp}`,
    profileId: adminProfileId,
    expiresAt: new Date(Date.now() + 86_400_000).toISOString(),
  });
  const ctx = await bearer(playwright, baseURL!, created.secret);
  try {
    const exported = await ctx.get("/api/v1/admin/config/export");
    expect(exported.status()).toBe(200);
    expect((await ctx.get("/api/v1/admin/profiles")).status()).toBe(200);
    expect((await ctx.get(`/api/v1/admin/profiles/${target.id}`)).status()).toBe(200);

    const config = await exported.json();
    const writes = [
      ctx.post("/api/v1/admin/profiles", { data: { name: `E2E by token ${stamp}` } }),
      ctx.patch(`/api/v1/admin/profiles/${target.id}`, { data: { globalPermissions: ["users.manage"] } }),
      ctx.patch(`/api/v1/admin/profiles/${adminProfileId}`, { data: { requireMfa: false } }),
      ctx.delete(`/api/v1/admin/profiles/${target.id}`),
      ctx.post(`/api/v1/admin/profiles/${target.id}/clone`, { data: { name: `E2E clone ${stamp}` } }),
      ctx.post("/api/v1/admin/config/import?mode=dry_run", { data: config }),
      ctx.post("/api/v1/admin/config/import?mode=apply", { data: config }),
    ];
    for (const res of await Promise.all(writes)) {
      expect(res.status(), `${res.url()} → ${res.status()}`).toBe(403);
      const { error } = await res.json();
      expect(error).toMatchObject({ code: "FORBIDDEN", message: SESSION_ONLY });
    }
  } finally {
    await ctx.dispose();
  }
  // Nothing changed, and nothing was created.
  expect(await apiGet<unknown>(request, `/admin/profiles/${target.id}`)).toEqual(before);
  const named = await apiGet<{ data: { name: string }[] }>(request, `/admin/profiles?limit=200`);
  expect(named.data.filter((p) => p.name === `E2E by token ${stamp}` || p.name === `E2E clone ${stamp}`)).toHaveLength(0);
  // Each refusal is audited as a session_only use of the token.
  const audit = await apiGet<{ data: { action: string; newValue: unknown }[] }>(request, `/audit-log?entityType=api_tokens&entityId=${created.token.id}&limit=50`);
  expect(audit.data.filter((e) => e.action === "token.use" && JSON.stringify(e.newValue).includes("session_only")).length).toBeGreaterThanOrEqual(7);
  await request.delete(`/api/v1/admin/api-tokens/${created.token.id}`, { headers: { "X-CSRF-Token": await csrf(request) } });
});

test("/usage needs datamodel.manage; without it the counts of hidden CIs are not reachable (GH#121)", async ({ request, playwright, baseURL }) => {
  const applicationId = await classIdByName(request, "Application");
  const databaseId = await classIdByName(request, "Database");
  const viewer = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E usage viewer ${stamp}`,
    globalPermissions: [],
    classPermissions: [{ classId: applicationId, view: true, create: false, edit: false, delete: false }],
  });
  profiles.push(viewer.id);
  const user = await createUser(request, "usage", [viewer.id]);
  const asUser = await signIn(playwright, baseURL!, user.username);
  const lists = await apiGet<{ data: { id: string; key: string }[] }>(request, "/lookup-lists?limit=100");
  const status = lists.data.find((l) => l.key === "status")!;
  for (const path of [`/ci-classes/${databaseId}/usage`, `/ci-classes/${applicationId}/usage`, `/lookup-lists/${status.id}/usage`]) {
    const res = await asUser.send("GET", path);
    expect(res.status(), `${path} → ${res.status()}`).toBe(403);
    expect((await res.json()).error.code).toBe("FORBIDDEN");
  }
  // The administrator, who holds datamodel.manage, still gets the counts.
  const usage = await apiGet<{ removal: string; data: { kind: string }[] }>(request, `/ci-classes/${databaseId}/usage`);
  expect(usage.removal).toBe("purge");
  expect(usage.data.map((u) => u.kind)).toContain("configurationItems");
  await asUser.ctx.dispose();
});
