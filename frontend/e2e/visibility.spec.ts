import { readFile } from "node:fs/promises";
import type { APIRequestContext, APIResponse, Browser, Page } from "@playwright/test";
import { apiGet, apiSend, expect, expectDialogLaidOut, snap, test } from "./support";

// The per-type visibility fixes of v0.1.1 (GH#261, GH#264-GH#269) as the users they protect against: an operator
// with the data-model permission and an auditor with audit.view, who may both view only the class "Public". Nothing
// the UI shows them (an audit total, a usage count, a refusal) may tell them anything about the CIs of "Secrets".
// The administrator sees the same screens with every count.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "visibility-password-123";
const WRITER = `e2e-vis-writer-${stamp}`;
const OPERATOR = `e2e-vis-operator-${stamp}`;
const AUDITOR = `e2e-vis-auditor-${stamp}`;
const PUB_CIS = [`vis-pub-one-${stamp}`, `vis-pub-two-${stamp}`];
const SEC_CI = `vis-secret-${stamp}`;
const REL_TYPE = `Vis link ${stamp}`;
const MAKER = `Vis maker ${stamp}`;
const MODEL = `Vis model ${stamp}`;

let publicId = "";
let secretsId = "";
let makerId = "";
let modelId = "";
let relTypeId = "";
let secretCiId = "";

/** Messages the operator gets may carry the stamp (in keys), never a count. */
const digitsBesidesStamp = (s: string) => /\d/.test(s.replaceAll(stamp, ""));

async function signInApi(playwright: { request: { newContext: (o: object) => Promise<APIRequestContext> } }, baseURL: string, username: string) {
  const ctx = await playwright.request.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const res = await ctx.post("/api/v1/auth/login", { data: { username, password: PASSWORD } });
  expect(res.status(), `sign in as ${username}: ${await res.text()}`).toBe(200);
  const csrf = (await res.json()).csrfToken as string;
  const send = (method: string, path: string, data?: unknown) => ctx.fetch(`/api/v1${path}`, { method, data, headers: { "X-CSRF-Token": csrf } });
  return { ctx, get: (path: string) => ctx.get(`/api/v1${path}`), send };
}

async function json<T>(res: APIResponse): Promise<T> {
  expect(res.ok(), `${res.url()} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return (await res.json()) as T;
}

async function signInUi(browser: Browser, username: string): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  return page;
}

test.beforeAll(async ({ request, playwright, baseURL }) => {
  // Two classes, each titled by its name and holding a "Model" lookup of a list that depends on "Maker".
  const cls = async (name: string, key: string) => {
    const { id } = await apiSend<{ id: string }>(request, "POST", "/ci-classes", { name, key });
    const title = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: id, key: "name", label: "Name", dataType: "text" });
    await apiSend(request, "PATCH", `/ci-classes/${id}`, { titleAttributeId: title.id });
    return id;
  };
  publicId = await cls(`Vis public ${stamp}`, `vis_public_${stamp}`);
  secretsId = await cls(`Vis secrets ${stamp}`, `vis_secrets_${stamp}`);
  makerId = (await apiSend<{ id: string }>(request, "POST", "/lookup-lists", { key: `vis_maker_${stamp}`, name: MAKER })).id;
  modelId = (await apiSend<{ id: string }>(request, "POST", "/lookup-lists", { key: `vis_model_${stamp}`, name: MODEL, parentListId: makerId })).id;
  const acme = await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: makerId, key: "acme", name: "Acme" });
  const rocket = await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: modelId, key: "rocket", name: "Rocket", parentValueId: acme.id });
  for (const classId of [publicId, secretsId]) {
    await apiSend(request, "POST", "/attribute-definitions", { classId, key: "model", label: "Model", dataType: "lookup", lookupListId: modelId });
  }
  relTypeId = (await apiSend<{ id: string }>(request, "POST", "/relationship-types", { key: `vis_link_${stamp}`, name: REL_TYPE, forwardLabel: "links to", reverseLabel: "linked from" })).id;
  for (const targetClassId of [publicId, secretsId]) {
    await apiSend(request, "POST", "/relationship-rules", { relationshipTypeId: relTypeId, sourceClassId: publicId, targetClassId });
  }

  const profile = async (name: string, globalPermissions: string[], classIds: string[], write: boolean) =>
    (
      await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
        name: `${name} ${stamp}`,
        globalPermissions,
        classPermissions: classIds.map((classId) => ({ classId, view: true, create: write, edit: write, delete: write })),
      })
    ).id;
  const user = async (username: string, profileId: string) =>
    apiSend(request, "POST", "/admin/users", { username, email: `${username}@example.test`, displayName: username, password: PASSWORD, profileIds: [profileId] });
  // The writer may use both classes; every CI and relationship below is theirs, so the audit log can be filtered to them.
  await user(WRITER, await profile("E2E vis writers", [], [publicId, secretsId], true));
  await user(OPERATOR, await profile("E2E vis operators", ["datamodel.manage"], [publicId], true));
  await user(AUDITOR, await profile("E2E vis auditors", ["audit.view"], [publicId], false));

  const writer = await signInApi(playwright, baseURL!, WRITER);
  const ci = async (classId: string, name: string, attributes: Record<string, unknown> = {}) =>
    (await json<{ id: string }>(await writer.send("POST", "/configuration-items", { classId, attributes: { name, ...attributes } }))).id;
  const pubOne = await ci(publicId, PUB_CIS[0]);
  const pubTwo = await ci(publicId, PUB_CIS[1]);
  secretCiId = await ci(secretsId, SEC_CI, { model: rocket.id });
  for (const target of [pubTwo, secretCiId]) {
    await json(await writer.send("POST", "/relationships", { relationshipTypeId: relTypeId, sourceCiId: pubOne, targetCiId: target }));
  }
  await writer.ctx.dispose();
});

test("audit log: a class-limited auditor's list and total leave out CIs they may not view (GH#264)", async ({ browser, playwright, baseURL, request }) => {
  const auditor = await signInApi(playwright, baseURL!, AUDITOR);
  const total = async (get: (p: string) => Promise<APIResponse>, query: string) =>
    (await json<{ page: { total: number } }>(await get(`/audit-log?${query}&limit=1`))).page.total;
  const adminGet = (p: string) => request.get(`/api/v1${p}`);
  try {
    // The writer created 3 CIs (one in Secrets) and 2 relationships (one to the Secrets CI).
    const cis = `actorName=${WRITER}&entityType=configuration_items`;
    const rels = `actorName=${WRITER}&entityType=ci_relationships`;
    expect([await total(adminGet, cis), await total(adminGet, rels)]).toEqual([3, 2]);
    expect([await total(auditor.get, cis), await total(auditor.get, rels)]).toEqual([2, 1]);
    // Filtering by the hidden CI's id or by its request tells nothing either.
    const [created] = (await apiGet<{ data: { requestId: string }[] }>(request, `/audit-log?entityId=${secretCiId}`)).data;
    for (const query of [`entityId=${secretCiId}`, `requestId=${created.requestId}`]) {
      expect(await total(adminGet, query), query).toBeGreaterThan(0);
      expect(await total(auditor.get, query), query).toBe(0);
    }
    // Nowhere in the auditor's view of the writer's history does the Secrets CI appear.
    const visible = JSON.stringify((await json<{ data: unknown[] }>(await auditor.get(`/audit-log?actorName=${WRITER}&limit=200`))).data);
    expect(visible).not.toContain(SEC_CI);
    expect(visible).not.toContain(secretCiId);
  } finally {
    await auditor.ctx.dispose();
  }

  // The UI shows the auditor the same total, and only the Public CIs.
  const page = await signInUi(browser, AUDITOR);
  await page.goto(`/admin/audit?actorName=${WRITER}&entityType=configuration_items`);
  const log = page.getByRole("region", { name: "Audit log" });
  await expect(page.locator(".list-head .count")).toHaveText("2 entries");
  await expect(log.locator("tbody tr")).toHaveCount(2);
  for (const name of PUB_CIS) await expect(log.getByRole("link", { name })).toBeVisible();
  await expect(log).not.toContainText(SEC_CI);
  await snap(page, "vis-01-audit-auditor");
  await page.context().close();

  // The administrator sees all three.
  const admin = await browser.newPage();
  await admin.goto(`/admin/audit?actorName=${WRITER}&entityType=configuration_items`);
  await expect(admin.locator(".list-head .count")).toHaveText("3 entries");
  await expect(admin.getByRole("region", { name: "Audit log" }).getByRole("link", { name: SEC_CI })).toBeVisible();
  await snap(admin, "vis-02-audit-admin");
  await admin.close();
});

test("data model: the delete dialog lists counts over hidden CIs without a number (GH#265)", async ({ browser, playwright, baseURL }) => {
  const operator = await signInApi(playwright, baseURL!, OPERATOR);
  try {
    type Usage = { inUse: boolean; data: { kind: string; count: number | null; withheld: boolean }[] };
    const usage = await json<Usage>(await operator.get(`/relationship-types/${relTypeId}/usage`));
    expect(usage.inUse).toBe(true);
    expect(usage.data.find((u) => u.kind === "relationships")).toMatchObject({ count: null, withheld: true });
    const refused = await operator.send("DELETE", `/relationship-types/${relTypeId}`);
    expect(refused.status()).toBe(409);
    const { error } = await refused.json();
    expect(error.code).toBe("IN_USE");
    expect(error.message).toContain("details withheld");
    expect(error.details).toEqual([]);
    expect(digitsBesidesStamp(error.message), error.message).toBe(false);
  } finally {
    await operator.ctx.dispose();
  }

  const withheld = /^Any .*\(number withheld: it spans CI types you may not view\)$/;
  const page = await signInUi(browser, OPERATOR);
  // A relationship type used between Public and Secrets CIs.
  await page.goto("/admin/relationships");
  await page.getByRole("button", { name: `Actions for ${REL_TYPE}` }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
  let dialog = page.getByRole("dialog", { name: `Delete relationship type “${REL_TYPE}”?` });
  await expect(dialog.getByText("It cannot be deleted while it is in use:")).toBeVisible();
  await expect(dialog.getByRole("listitem").filter({ hasText: /relationships/i }).first()).toHaveText(withheld);
  for (const item of await dialog.getByRole("listitem").allTextContents()) expect(digitsBesidesStamp(item), item).toBe(false);
  await expectDialogLaidOut(dialog);
  await snap(page, "vis-03-delete-dialog-withheld");
  await dialog.getByRole("button", { name: "Cancel" }).click();

  // A lookup value stored only on a Secrets CI.
  await page.goto(`/admin/dropdowns?list=${modelId}`);
  const values = page.getByRole("region", { name: `Values of “${MODEL}”` });
  await values.getByRole("button", { name: "Actions for Rocket" }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
  dialog = page.getByRole("dialog", { name: "Delete value “Rocket”?" });
  await expect(dialog.getByText("It cannot be deleted while it is in use:")).toBeVisible();
  await expect(dialog.getByRole("listitem")).toHaveText([withheld]);
  await expectDialogLaidOut(dialog);
  await snap(page, "vis-04-lookup-delete-withheld");
  await page.context().close();

  // The administrator gets the number.
  const admin = await browser.newPage();
  await admin.goto(`/admin/dropdowns?list=${modelId}`);
  await admin.getByRole("region", { name: `Values of “${MODEL}”` }).getByRole("button", { name: "Actions for Rocket" }).click();
  await admin.getByRole("menuitem", { name: "Delete" }).click();
  await expect(admin.getByRole("dialog", { name: "Delete value “Rocket”?" }).getByRole("listitem")).toHaveText([/^1 /]);
  await admin.close();
});

test("lookups: refusing to retire a value names the dependent value without a count (GH#266)", async ({ browser, request }) => {
  const page = await signInUi(browser, OPERATOR);
  await page.goto(`/admin/dropdowns?list=${makerId}`);
  const values = page.getByRole("region", { name: `Values of “${MAKER}”` });
  await values.getByRole("button", { name: "Actions for Acme" }).click();
  await page.getByRole("menuitem", { name: "Archive" }).click();
  const alert = values.getByRole("alert");
  await expect(alert).toContainText("Not saved");
  await expect(alert).toContainText(`vis_model_${stamp}.rocket`);
  const refusal = ((await alert.textContent()) ?? "").split("Request id")[0];
  expect(digitsBesidesStamp(refusal), refusal).toBe(false);
  await snap(page, "vis-05-retire-refused-operator");
  await page.context().close();

  // The administrator is told how many CIs, and the value is still active.
  const admin = await browser.newPage();
  await admin.goto(`/admin/dropdowns?list=${makerId}`);
  const adminValues = admin.getByRole("region", { name: `Values of “${MAKER}”` });
  await adminValues.getByRole("button", { name: "Actions for Acme" }).click();
  await admin.getByRole("menuitem", { name: "Archive" }).click();
  await expect(adminValues.getByRole("alert")).toContainText(`vis_model_${stamp}.rocket (1 configuration items)`);
  await admin.close();
  const acme = (await apiGet<{ data: { name: string; isActive: boolean }[] }>(request, `/lookup-list-values?listId=${makerId}`)).data;
  expect(acme).toEqual([expect.objectContaining({ name: "Acme", isActive: true })]);
});

test("inventory export: a class-limited user's file holds only the CIs they may view (SHAA-2353)", async ({ browser }) => {
  const page = await signInUi(browser, AUDITOR);
  await page.goto("/cis");
  const exportButton = page.locator(".page-header .actions .row-menu > button");
  await expect(exportButton).toHaveText("Export");
  await exportButton.click();
  const download = page.waitForEvent("download");
  await page.getByRole("menuitem", { name: "CSV, comma-separated" }).click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(/^inventory-.*\.csv$/);
  const csv = await readFile(await file.path(), "utf8");
  for (const name of PUB_CIS) expect(csv).toContain(`"${name}"`);
  expect(csv).not.toContain(SEC_CI);
  expect(csv).not.toContain(secretCiId);
  // Every data row is one of the two Public CIs: nothing of any other class.
  expect(csv.replace(/^\uFEFF/, "").split("\r\n").filter(Boolean)).toHaveLength(1 + PUB_CIS.length);
  await page.context().close();
});
