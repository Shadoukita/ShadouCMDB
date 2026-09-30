import type { APIRequestContext, APIResponse, Browser, Page } from "@playwright/test";
import { apiGet, apiSend, at, ciIdByName, classIdByName, expect, lookupValueId, snap, test } from "./support";

// What a restricted user can reach, checked at the API as well as in the UI: the UI hiding a button is
// not the claim under test, the server refusing the call is. Every refused write is re-read as the
// administrator to prove nothing changed.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PROFILE = `E2E app editors ${stamp}`;
const USERNAME = `e2e-restricted-${stamp}`;
const PASSWORD = "restricted-password-123";

let applicationId = "";
let serverId = "";
let databaseId = "";
let statusId = "";
let profileId = "";
let userId = "";
let restricted: ApiSession;

/** An API client signed in as one user, sending the CSRF token on writes like the web UI does. */
interface ApiSession {
  ctx: APIRequestContext;
  csrf: string;
  send(method: "POST" | "PATCH" | "PUT" | "DELETE", path: string, data?: unknown, csrf?: string | null): Promise<APIResponse>;
  get(path: string): Promise<APIResponse>;
}

async function apiSignIn(playwright: { request: { newContext: (o: object) => Promise<APIRequestContext> } }, baseURL: string, username: string, password: string): Promise<ApiSession> {
  const ctx = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const res = await ctx.post("/api/v1/auth/login", { data: { username, password } });
  expect(res.status(), `sign in as ${username}: ${await res.text()}`).toBe(200);
  const csrf = (await res.json()).csrfToken as string;
  return {
    ctx,
    csrf,
    get: (path) => ctx.get(`/api/v1${path}`),
    send: (method, path, data, token = csrf) =>
      ctx.fetch(`/api/v1${path}`, { method, data, headers: token === null ? {} : { "X-CSRF-Token": token } }),
  };
}

async function expectError(res: APIResponse, status: number, code: string) {
  expect(res.status(), `${res.url()} → ${res.status()} ${await res.text()}`).toBe(status);
  expect((await res.json()).error.code).toBe(code);
}

async function signInUi(browser: Browser, username: string, password: string): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  return page;
}

test.beforeAll(async ({ request, playwright, baseURL }) => {
  applicationId = await classIdByName(request, "Application");
  serverId = await classIdByName(request, "Server");
  databaseId = await classIdByName(request, "Database");
  statusId = await lookupValueId(request, "status", "in_service");
  // No global permission at all. Applications: view, create, edit (no delete). Servers: view only. Databases: nothing.
  profileId = (
    await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
      name: PROFILE,
      globalPermissions: [],
      classPermissions: [
        { classId: applicationId, view: true, create: true, edit: true, delete: false },
        { classId: serverId, view: true, create: false, edit: false, delete: false },
      ],
    })
  ).id;
  userId = (
    await apiSend<{ id: string }>(request, "POST", "/admin/users", {
      username: USERNAME,
      displayName: `E2E Restricted ${stamp}`,
      password: PASSWORD,
      profileIds: [profileId],
    })
  ).id;
  restricted = await apiSignIn(playwright, baseURL!, USERNAME, PASSWORD);
});

test.afterAll(async () => {
  await restricted?.ctx.dispose();
});

test("anonymous callers get 401 from every protected endpoint", async ({ playwright, baseURL }) => {
  const anon = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  for (const path of ["/configuration-items", "/ci-classes", "/admin/users", "/admin/profiles", "/audit-log", "/auth/me"]) {
    await expectError(await anon.get(`/api/v1${path}`), 401, "UNAUTHENTICATED");
  }
  await expectError(await anon.post("/api/v1/admin/users", { data: { username: "x", displayName: "x", password: "long-enough-pw" } }), 401, "UNAUTHENTICATED");
  // Setup is closed once a user exists, and cannot be used to mint a second administrator.
  expect((await (await anon.get("/api/v1/setup")).json()).setupRequired).toBe(false);
  await expectError(
    await anon.post("/api/v1/setup", { data: { username: `sneaky-${stamp}`, displayName: "x", password: "long-enough-pw", setupToken: "guessed" } }),
    409,
    "CONFLICT",
  );
  await anon.dispose();
});

test("the session reports exactly the profile's permissions", async () => {
  const me = await (await restricted.get("/auth/me")).json();
  expect(me.user.username).toBe(USERNAME);
  expect(me.permissions.global).toEqual([]);
  expect(me.permissions.administrator).toBe(false);
});

test("a restricted user cannot call any administration endpoint", async ({ request }) => {
  const adminProfile = (await apiGet<{ data: { id: string; isBuiltin: boolean }[] }>(request, "/admin/profiles?limit=200")).data.find((p) => p.isBuiltin)!;
  const me = await (await restricted.get("/auth/me")).json();

  const refused: [string, () => Promise<APIResponse>][] = [
    ["list users", () => restricted.get("/admin/users")],
    ["read a user", () => restricted.get(`/admin/users/${userId}`)],
    ["create a user", () => restricted.send("POST", "/admin/users", { username: `x-${stamp}`, displayName: "x", password: "long-enough-pw", profileIds: [adminProfile.id] })],
    ["grant themselves Administrator", () => restricted.send("PATCH", `/admin/users/${me.user.id}`, { profileIds: [adminProfile.id] })],
    ["reset a password", () => restricted.send("PUT", `/admin/users/${userId}/password`, { password: "hijacked-password-1" })],
    ["list profiles", () => restricted.get("/admin/profiles")],
    ["create a profile", () => restricted.send("POST", "/admin/profiles", { name: `x-${stamp}`, globalPermissions: ["users.manage"], classPermissions: [] })],
    ["widen their own profile", () => restricted.send("PATCH", `/admin/profiles/${profileId}`, { globalPermissions: ["users.manage"] })],
    ["delete a profile", () => restricted.send("DELETE", `/admin/profiles/${profileId}`)],
    ["read the audit log", () => restricted.get("/audit-log")],
    ["create a CI class", () => restricted.send("POST", "/ci-classes", { key: `x_${stamp}`, name: `X ${stamp}` })],
    ["rename a CI class", () => restricted.send("PATCH", `/ci-classes/${applicationId}`, { name: "Renamed by a restricted user" })],
    ["add an attribute", () => restricted.send("POST", "/attribute-definitions", { classId: applicationId, key: `x_${stamp}`, name: "X", dataType: "string" })],
    ["create a status", () => restricted.send("POST", "/statuses", { key: `x_${stamp}`, name: `X ${stamp}` })],
  ];
  for (const [what, call] of refused) {
    const res = await call();
    expect(res.status(), `${what}: ${res.url()} → ${res.status()} ${await res.text()}`).toBe(403);
    expect((await res.json()).error.code, what).toBe("FORBIDDEN");
  }

  // Nothing changed, read back as the administrator.
  const profile = await apiGet<{ globalPermissions: string[] }>(request, `/admin/profiles/${profileId}`);
  expect(profile.globalPermissions).toEqual([]);
  const user = await apiGet<{ profiles: { id: string }[] }>(request, `/admin/users/${me.user.id}`);
  expect(user.profiles.map((p) => p.id)).toEqual([profileId]);
  expect((await apiGet<{ name: string }>(request, `/ci-classes/${applicationId}`)).name).toBe("Application");
  const users = await apiGet<{ data: { username: string }[] }>(request, `/admin/users?q=x-${stamp}`);
  expect(users.data).toEqual([]);
});

test("class permissions are enforced per class and per operation", async ({ request }) => {
  const crmDb = await ciIdByName(request, "crm-db");
  const esx = await ciIdByName(request, "fra1-esx-01");

  // View: lists only hold the classes the user may view; a CI of any other class answers 404 by id, like a missing one.
  const list = (await (await restricted.get("/configuration-items?limit=200")).json()) as { data: { classId: string }[] };
  expect(list.data.length).toBeGreaterThan(0);
  expect(new Set(list.data.map((c) => c.classId))).toEqual(new Set([applicationId, serverId]));
  const databases = await restricted.get(`/configuration-items?classId=${databaseId}`);
  expect(databases.status()).toBe(200);
  expect((await databases.json()).data).toEqual([]);
  await expectError(await restricted.get(`/configuration-items/${crmDb}`), 404, "NOT_FOUND");
  expect((await restricted.get(`/configuration-items/${esx}`)).status()).toBe(200);

  // Create: allowed for Application only.
  const created = await restricted.send("POST", "/configuration-items", { classId: applicationId, attributes: { name: `e2e-app-${stamp}`, status: statusId } });
  expect(created.status(), await created.text()).toBe(201);
  const app = (await created.json()) as { id: string; version: number };
  await expectError(await restricted.send("POST", "/configuration-items", { classId: serverId, attributes: { name: `e2e-srv-${stamp}`, status: statusId } }), 403, "FORBIDDEN");
  await expectError(await restricted.send("POST", "/configuration-items", { classId: databaseId, attributes: { name: `e2e-db-${stamp}`, status: statusId, engine: "postgresql" } }), 403, "FORBIDDEN");

  // Edit: allowed for Application, refused for Server (view only) and Database (nothing).
  const edited = await restricted.send("PATCH", `/configuration-items/${app.id}`, { attributes: { notes: "edited by the restricted user" }, version: app.version });
  expect(edited.status(), await edited.text()).toBe(200);
  const esxBefore = await apiGet<{ attributes: Record<string, unknown>; version: number }>(request, `/configuration-items/${esx}`);
  await expectError(await restricted.send("PATCH", `/configuration-items/${esx}`, { attributes: { notes: "should not stick" }, version: esxBefore.version }), 403, "FORBIDDEN");
  const dbBefore = await apiGet<{ version: number }>(request, `/configuration-items/${crmDb}`);
  await expectError(await restricted.send("PATCH", `/configuration-items/${crmDb}`, { attributes: { notes: "should not stick" }, version: dbBefore.version }), 404, "NOT_FOUND");

  // Delete: refused everywhere (the profile grants no delete).
  await expectError(await restricted.send("DELETE", `/configuration-items/${app.id}`), 403, "FORBIDDEN");
  await expectError(await restricted.send("DELETE", `/configuration-items/${esx}`), 403, "FORBIDDEN");

  // Read back as the administrator: only the permitted writes landed.
  expect((await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${app.id}`)).attributes.notes).toBe("edited by the restricted user");
  const esxAfter = await apiGet<{ attributes: Record<string, unknown>; version: number }>(request, `/configuration-items/${esx}`);
  expect({ attributes: esxAfter.attributes, version: esxAfter.version }).toEqual({ attributes: esxBefore.attributes, version: esxBefore.version });
  expect((await apiGet<{ version: number }>(request, `/configuration-items/${crmDb}`)).version).toBe(dbBefore.version);
  const servers = await apiGet<{ data: unknown[] }>(request, `/configuration-items?q=e2e-srv-${stamp}`);
  expect(servers.data).toEqual([]);
});

test("writes without the session's CSRF token are refused", async () => {
  const body = { classId: applicationId, attributes: { name: `e2e-csrf-${stamp}`, status: statusId } };
  await expectError(await restricted.send("POST", "/configuration-items", body, null), 403, "CSRF_TOKEN_INVALID");
  await expectError(await restricted.send("POST", "/configuration-items", body, "not-the-token"), 403, "CSRF_TOKEN_INVALID");
});

test("the UI shows a restricted user only what they may do", async ({ browser, request }) => {
  const page = await signInUi(browser, USERNAME, PASSWORD);
  const nav = page.getByRole("navigation", { name: "Main" });
  // No global permission: no Administration entry at all, and the pages refuse.
  await expect(nav.getByRole("link", { name: "Administration", exact: true })).toHaveCount(0);
  for (const path of ["/admin", "/admin/users", "/admin/profiles/new", "/admin/audit"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { name: "You do not have access to Administration" }), path).toBeVisible();
    await expect(page.getByRole("navigation", { name: "Administration" }), path).toHaveCount(0);
  }

  // "Browse by class" lists only the classes the user may view, so no class shows a false count of 0.
  await expect(nav.getByRole("link", { name: /^Application\b/ })).toBeVisible();
  await expect(nav.getByRole("link", { name: /^Server\b/ })).toBeVisible();
  await expect(nav.getByRole("link", { name: /^Database\b/ })).toHaveCount(0);
  await expect(nav.getByRole("link", { name: /^Location\b/ })).toHaveCount(0);
  // A class the user may not view, opened by URL, explains itself instead of looking empty.
  await page.goto(`/cis?classId=${databaseId}`);
  await expect(page.getByRole("heading", { name: "Permission denied" })).toBeVisible();
  await expect(page.getByText("None of your permission profiles allows viewing Database")).toBeVisible();
  // The class filter offers the viewable classes and their abstract parents (Hardware lists Servers), plus the one in the URL.
  // Roots follow the classes' sort order, so only the denied class's place and Server under Hardware are fixed.
  const classOptions = page.locator("#f-class option");
  await expect(classOptions).toHaveCount(5);
  await expect(classOptions.nth(0)).toHaveText("All classes");
  await expect(classOptions.nth(1)).toHaveText("Database");
  const optionTexts = (await classOptions.allTextContents()).map((t) => t.trim());
  expect([...optionTexts].sort()).toEqual(["All classes", "Application", "Database", "Hardware (incl. subclasses)", "Server"]);
  expect(optionTexts.indexOf("Server")).toBe(optionTexts.indexOf("Hardware (incl. subclasses)") + 1);

  // "New CI" offers only the classes the profile may create.
  await page.goto("/");
  await page.getByRole("banner").getByRole("link", { name: "New CI", exact: true }).click();
  await expect(page.locator("#ci-class option")).toHaveText(["Choose a class…", "Application"]);
  // A class the user may not create in, opened by URL, is refused with an explanation.
  await page.goto(`/cis/new?classId=${databaseId}`);
  await expect(page.getByRole("alert")).toContainText("None of your permission profiles allows creating Database");

  // The inventory never lists a CI the user may not view.
  await page.goto("/cis?q=crm");
  await expect(page.getByRole("link", { name: "CRM", exact: true })).toBeVisible();
  await expect(page.getByRole("link", { name: "crm-db", exact: true })).toHaveCount(0);
  await snap(page, "30-restricted-inventory");

  // A Server: readable, no Edit, no Delete; the edit URL refuses.
  const esx = await ciIdByName(request, "fra1-esx-01");
  await page.goto(`/cis/${esx}`);
  await expect(page.getByRole("heading", { level: 1, name: "fra1-esx-01" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Edit", exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0);
  await expect(page.getByRole("tab", { name: "History" })).toHaveCount(0); // no audit.view
  await page.goto(`/cis/${esx}/edit`);
  await expect(page.getByRole("heading", { name: "Permission denied" })).toBeVisible();

  // A Database CI opened by URL: not rendered, and indistinguishable from a
  // missing id (the API answers 404 so its existence is not revealed).
  await page.goto(`/cis/${await ciIdByName(request, "crm-db")}`);
  await expect(page.getByRole("heading", { name: "Configuration item not found" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Not found");
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).not.toContainText("Permission denied");
  await expect(page.getByRole("button", { name: "Retry" })).toHaveCount(0);
  await expect(page.getByRole("heading", { level: 1, name: "crm-db" })).toHaveCount(0);
  await snap(page, "31-restricted-denied");

  // An Application: Edit is offered and works end to end; Delete is not offered.
  const app = await ciIdByName(request, `e2e-app-${stamp}`);
  await page.goto(`/cis/${app}`);
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0);
  await page.getByRole("link", { name: "Edit", exact: true }).click();
  // The ident is generated and only administrators may change it.
  await expect(page.locator("#f-ident")).toBeDisabled();
  await page.locator("#attr-version").fill(`e2e-${stamp}`);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(at(`/cis/${app}`));
  expect((await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${app}`)).attributes.version).toBe(`e2e-${stamp}`);
  await page.context().close();
});

test("a reference into a class the user may not view shows a placeholder, not a name or a link", async ({ browser, page, request }) => {
  // CRM (Application) → primary_database crm-db (Database, which the profile cannot see).
  const crm = await ciIdByName(request, "CRM");
  // Show the attribute as an inventory column for Application; the settings are restored at the end.
  const before = await apiGet<{ version: number; settings: { listViews: { classKey: string }[] } }>(request, "/ui-settings");
  const listViews = [...before.settings.listViews.filter((v) => v.classKey !== "application"), { classKey: "application", columns: ["label", "attributes.primary_database"] }];
  const saved = await apiSend<{ version: number }>(request, "PUT", "/ui-settings", { version: before.version, settings: { ...before.settings, listViews } });

  try {
    // The administrator still sees the name, as a link.
    await page.goto(`/cis/${crm}`);
    await expect(page.getByRole("link", { name: "crm-db", exact: true }).first()).toBeVisible();

    const restrictedPage = await signInUi(browser, USERNAME, PASSWORD);
    const hiddenIn = async (where: string) => {
      await expect(restrictedPage.getByText("Hidden CI", { exact: true }).first(), where).toBeVisible();
      await expect(restrictedPage.getByRole("link", { name: "Hidden CI" }), where).toHaveCount(0);
      await expect(restrictedPage.getByText("crm-db"), where).toHaveCount(0);
    };
    await restrictedPage.goto(`/cis?classId=${applicationId}&q=CRM`);
    await expect(restrictedPage.getByRole("link", { name: "CRM", exact: true })).toBeVisible();
    await hiddenIn("inventory");
    await restrictedPage.goto(`/cis/${crm}`);
    await expect(restrictedPage.getByRole("heading", { level: 1, name: "CRM" })).toBeVisible();
    await hiddenIn("detail");
    await restrictedPage.getByRole("link", { name: "Edit", exact: true }).click();
    await expect(restrictedPage.getByRole("button", { name: "Clear Hidden CI" })).toBeVisible();
    await hiddenIn("edit");
    // Saving other fields keeps the hidden reference as it was.
    await restrictedPage.locator("#attr-notes").fill(`edited around a hidden reference ${stamp}`);
    await restrictedPage.getByRole("button", { name: "Save changes" }).click();
    await expect(restrictedPage).toHaveURL(at(`/cis/${crm}`));
    await restrictedPage.context().close();
    const after = await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${crm}`);
    expect(after.attributes.primary_database).toBe(await ciIdByName(request, "crm-db"));
  } finally {
    await apiSend(request, "PUT", "/ui-settings", { version: saved.version, settings: before.settings });
  }
});

test("audit.view shows a class-restricted auditor no values of CIs they may not view", async ({ playwright, baseURL, request }) => {
  // An auditor with audit.view who may view Applications only. CRM (Application) references crm-db (Database).
  const crm = await ciIdByName(request, "CRM");
  const crmDb = await ciIdByName(request, "crm-db");
  const auditorProfile = (
    await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
      name: `E2E auditors ${stamp}`,
      globalPermissions: ["audit.view"],
      classPermissions: [{ classId: applicationId, view: true, create: false, edit: false, delete: false }],
    })
  ).id;
  const auditorName = `e2e-auditor-${stamp}`;
  await apiSend(request, "POST", "/admin/users", { username: auditorName, displayName: `E2E Auditor ${stamp}`, password: PASSWORD, profileIds: [auditorProfile] });
  const auditor = await apiSignIn(playwright, baseURL!, auditorName, PASSWORD);

  // Fresh audit rows for both CIs and for an edge between them, written by the administrator.
  const secret = `audit-secret-${stamp}`;
  for (const id of [crmDb, crm]) {
    const { version } = await apiGet<{ version: number }>(request, `/configuration-items/${id}`);
    await apiSend(request, "PATCH", `/configuration-items/${id}`, { attributes: { notes: secret }, version });
  }
  const edge = (await apiGet<{ data: { id: string; notes: string | null }[] }>(request, `/relationships?sourceCiId=${crm}&targetCiId=${crmDb}`)).data[0]
    ?? (await apiGet<{ data: { id: string; notes: string | null }[] }>(request, `/relationships?ciId=${crmDb}`)).data[0];
  expect(edge, "a relationship with crm-db in the demo data").toBeTruthy();
  await apiSend(request, "PATCH", `/relationships/${edge.id}`, { notes: secret });

  type Entry = { entityId: string; oldValue: Record<string, unknown> | null; newValue: Record<string, unknown> | null; redacted: boolean };
  const page = async (who: ApiSession, query: string) => {
    const res = await who.get(`/audit-log?${query}&limit=200`);
    expect(res.status(), await res.text()).toBe(200);
    return (await res.json()) as { data: Entry[]; page: { total: number } };
  };
  const log = async (who: ApiSession, query: string) => (await page(who, query)).data;
  try {
    // The CI itself stays hidden (404, as for an unknown id), and so does its history: its audit entries and those of
    // its relationships are neither listed nor counted (GH#264), so the total reveals nothing about them either.
    await expectError(await auditor.get(`/configuration-items/${crmDb}`), 404, "NOT_FOUND");
    for (const query of [`entityType=configuration_items&entityId=${crmDb}`, `entityType=ci_relationships&entityId=${edge.id}`]) {
      expect((await apiGet<{ page: { total: number } }>(request, `/audit-log?${query}`)).page.total).toBeGreaterThan(0);
      expect(await page(auditor, query)).toMatchObject({ data: [], page: { total: 0 } });
    }
    // Nowhere in the whole CI and relationship history does the secret or crm-db's name appear.
    const everything = JSON.stringify([...(await log(auditor, "entityType=configuration_items")), ...(await log(auditor, "entityType=ci_relationships"))]);
    expect(everything).not.toContain("crm-db");
    expect(everything.split(secret).length - 1).toBe(1); // CRM's own update, which the auditor may see

    // An Application the auditor may view keeps its values; its reference into Databases keeps only the id.
    const [crmRow] = await log(auditor, `entityType=configuration_items&entityId=${crm}`);
    expect(crmRow.redacted).toBe(false);
    expect((crmRow.newValue!.attributes as Record<string, unknown>).notes).toBe(secret);
    expect((crmRow.newValue!.attributeReferences as Record<string, unknown>).primary_database).toEqual({ id: crmDb, name: null, deleted: false, hidden: true });

    // The administrator still sees everything.
    const [adminDbRow] = (await apiGet<{ data: Entry[] }>(request, `/audit-log?entityType=configuration_items&entityId=${crmDb}`)).data;
    expect(adminDbRow.redacted).toBe(false);
    expect((adminDbRow.newValue!.attributes as Record<string, unknown>).notes).toBe(secret);
  } finally {
    await auditor.ctx.dispose();
  }
});

test("signing out ends the session on the server, not only in the browser", async ({ playwright, baseURL }) => {
  const s = await apiSignIn(playwright, baseURL!, USERNAME, PASSWORD);
  const cookies = (await s.ctx.storageState()).cookies;
  expect((await s.get("/auth/me")).status()).toBe(200);
  expect((await s.send("POST", "/auth/logout")).status()).toBeLessThan(300);

  // Replay the old cookies from a fresh client: the server must no longer know the session.
  const replay = await playwright.request.newContext({ baseURL, storageState: { cookies, origins: [] } });
  await expectError(await replay.get("/api/v1/auth/me"), 401, "UNAUTHENTICATED");
  await expectError(await replay.get("/api/v1/configuration-items"), 401, "UNAUTHENTICATED");
  await replay.dispose();
  await s.ctx.dispose();
});

test("disabling a user or resetting their password ends their open session in the UI", async ({ browser, request }) => {
  const page = await signInUi(browser, USERNAME, PASSWORD);
  await page.goto("/cis?q=fra1");
  await expect(page.getByRole("link", { name: "fra1-esx-01", exact: true })).toBeVisible();

  // Disabled by an administrator while signed in: the next request bounces to sign-in, and sign-in is refused.
  await apiSend(request, "PATCH", `/admin/users/${userId}`, { isActive: false });
  await page.locator("#f-q").fill("crm");
  await expect(page).toHaveURL(/\/login\?redirect=/);
  await expect(page.getByRole("status")).toContainText("Your session has ended");
  await page.getByLabel("Username").fill(USERNAME);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("alert")).toContainText("disabled");
  await expect(page).toHaveURL(/\/login/);

  // Re-enabled: sign-in works again and returns to the page the user was on.
  await apiSend(request, "PATCH", `/admin/users/${userId}`, { isActive: true });
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).toHaveURL(at("/cis", "?q=crm"));

  // A password reset by an administrator also ends the session; only the new password signs in.
  const newPassword = "reset-by-admin-456";
  const reset = await request.fetch(`/api/v1/admin/users/${userId}/password`, {
    method: "PUT",
    data: { password: newPassword },
    headers: { "X-CSRF-Token": (await request.storageState()).cookies.find((c) => c.name === "shadoucmdb_csrf")!.value },
  });
  expect(reset.status(), await reset.text()).toBeLessThan(300);
  await page.reload();
  await expect(page).toHaveURL(/\/login\?redirect=/);
  await page.getByLabel("Username").fill(USERNAME);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("alert")).toContainText("Wrong username or password");
  await page.getByLabel("Password").fill(newPassword);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).toHaveURL(at("/cis", "?q=crm"));
  await page.context().close();
});

test("repeated wrong passwords lock the username with 429 and Retry-After", async ({ playwright, baseURL, page }) => {
  // A username of its own, so the lock cannot disturb other tests.
  const victim = `e2e-lockout-${stamp}`;
  const anon = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  let locked: APIResponse | undefined;
  for (let i = 0; i < 8 && !locked; i++) {
    const res = await anon.post("/api/v1/auth/login", { data: { username: victim, password: `wrong-${i}` } });
    if (res.status() === 429) locked = res;
    else await expectError(res, 401, "UNAUTHENTICATED");
  }
  expect(locked, "no 429 after 8 failures").toBeTruthy();
  expect((await locked!.json()).error.code).toBe("RATE_LIMITED");
  expect(Number(locked!.headers()["retry-after"])).toBeGreaterThan(0);
  await anon.dispose();

  // The UI explains the lock instead of claiming the password is wrong.
  // The first lock lasts 1 s and can lapse before the page is up; a lapsed
  // attempt fails again and doubles the lock, so the next submit lands in it.
  await page.context().clearCookies();
  await page.goto("/login");
  await page.getByLabel("Username").fill(victim);
  await expect(async () => {
    await page.getByLabel("Password").fill("wrong-again");
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page.getByRole("alert")).toContainText(/too many|try again/i, { timeout: 2_000 });
  }).toPass({ timeout: 15_000 });
});

test("concurrent wrong passwords cannot get past the per-username limit (GH#118)", async ({ playwright, baseURL }) => {
  // Before the fix, every request that arrived before the first failure was counted had its password checked.
  const victim = `e2e-burst-${stamp}`;
  const anon = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const burst = await Promise.all(
    Array.from({ length: 30 }, (_, i) => anon.post("/api/v1/auth/login", { data: { username: victim, password: `wrong-${i}` } })),
  );
  const checked = burst.filter((r) => r.status() === 401);
  const refused = burst.filter((r) => r.status() === 429);
  expect(checked.length + refused.length, "only 401 and 429").toBe(30);
  // 5 free failures, plus one more attempt that then locks the name when the burst is spread out.
  expect(checked.length).toBeGreaterThan(0);
  expect(checked.length).toBeLessThanOrEqual(6);
  for (const res of refused) {
    expect(Number(res.headers()["retry-after"])).toBeGreaterThan(0);
    const { error } = await res.json();
    expect(error.code).toBe("RATE_LIMITED");
    expect(error.message).toMatch(/^Too many failed sign-ins for this username\. Try again in \d+ s\.$/);
  }
  await anon.dispose();
});
