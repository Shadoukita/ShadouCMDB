import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, expect, snap, test } from "./support";

// The barebone CI core every class shares: a General section with ident, valid from and valid
// until, then the class's own attributes; validity decides whether a CI is active; date inputs
// take "now" on double-click; only administrators may change an ident; the class's title
// attribute labels its CIs. The class is built through the API with no name attribute of its own.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const CLASS = `Core fields ${stamp}`;
const USERNAME = `e2e-core-${stamp}`;
const PASSWORD = "core-fields-password-1";
let classId = "";
let codeAttrId = "";
let scheduledId = "";
let expiredId = "";

const DAY = 24 * 60 * 60 * 1000;
const iso = (offsetMs: number) => new Date(Date.now() + offsetMs).toISOString();
/** The browser's local date and time as a date / datetime-local input shows it. */
const localNow = (page: Page) =>
  page.evaluate(() => {
    const d = new Date();
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
  });
/** Minutes between a datetime-local value and the browser's clock. */
const minutesFromNow = (page: Page, value: string) => page.evaluate((v) => Math.abs(Date.now() - new Date(v).getTime()) / 60_000, value);

test.beforeAll(async ({ request }) => {
  classId = (await apiSend<{ id: string }>(request, "POST", "/ci-classes", { key: `core_fields_${stamp}`, name: CLASS })).id;
  const attr = (body: Record<string, unknown>) => apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId, ...body });
  codeAttrId = (await attr({ key: "code", label: "Code", dataType: "text", sortOrder: 10 })).id;
  await attr({ key: "purchased", label: "Purchased", dataType: "date", groupName: "Lifecycle", sortOrder: 20 });
  await attr({ key: "go_live", label: "Go-live", dataType: "datetime", groupName: "Lifecycle", sortOrder: 30 });
});

test("the class page picks the title attribute that labels its CIs", async ({ page, request }) => {
  await page.goto(`/admin/classes/${classId}`);
  await expect(page.locator("#class-title")).toHaveValue("");
  await page.locator("#class-title").selectOption({ label: "Code (code)" });
  await page.getByRole("button", { name: "Save class" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  const cls = await apiGet<{ titleAttributeId: string | null }>(request, `/ci-classes/${classId}`);
  expect(cls.titleAttributeId).toBe(codeAttrId);
});

test("new CI: General first, valid from is now, double-click fills dates in", async ({ page, request }) => {
  await page.goto(`/cis/new?classId=${classId}`);
  // General (core fields and the ungrouped Code), then the class's group; no "Other".
  await expect(page.locator("form .layout-panel > summary h2")).toHaveText(["General", "Lifecycle"]);
  const general = page.locator("form .layout-panel").first();
  await expect(general.locator("label")).toHaveText(["Ident", /^Valid from/, "Valid until", "Criticality", "Code"]);

  // Valid from starts at the moment the form opened, in local time; valid until is open-ended.
  const from = await page.locator("#f-valid-from").inputValue();
  expect(from).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/);
  expect(await minutesFromNow(page, from)).toBeLessThan(2);
  await expect(page.locator("#f-valid-until")).toHaveValue("");

  // Double-click sets the current local date and time: core fields and date/datetime attributes alike.
  await page.locator("#f-valid-until").dblclick();
  expect(await minutesFromNow(page, await page.locator("#f-valid-until").inputValue())).toBeLessThan(2);
  await page.locator("#f-valid-until").fill("");
  await page.locator("#attr-purchased").dblclick();
  await expect(page.locator("#attr-purchased")).toHaveValue((await localNow(page)).slice(0, 10));
  await page.locator("#attr-go_live").dblclick();
  expect(await minutesFromNow(page, await page.locator("#attr-go_live").inputValue())).toBeLessThan(2);
  await expect(page.locator("#attr-go_live")).toHaveAttribute("title", /Double-click/);

  const code = `core-${stamp}-a`;
  await page.locator("#attr-code").fill(code);
  await snap(page, "core-fields-form");
  await page.getByRole("button", { name: `Create ${CLASS}` }).click();

  // Labelled by the title attribute; active, open-ended.
  await expect(page).toHaveURL(/\/cis\/[0-9a-f-]{36}$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(code);
  const ci = await apiGet<{ label: string; validUntil: string | null; active: boolean; attributes: Record<string, unknown> }>(
    request,
    `/configuration-items/${page.url().split("/").pop()}`,
  );
  expect(ci).toMatchObject({ label: code, validUntil: null, active: true });
  expect(ci.attributes.purchased).toBe((await localNow(page)).slice(0, 10));
  const general2 = page.locator(".layout-panels > details").first();
  await expect(general2.locator("dt")).toHaveText(["Ident", "Valid from", "Valid until", "Active", "Criticality", "Code"]);
  await expect(general2.locator("dt", { hasText: "Valid until" }).locator("+ dd")).toHaveText("Open-ended");
  await expect(general2.locator("dt", { hasText: "Criticality" }).locator("+ dd")).toHaveText("Not set");
  await expect(general2.locator("dt", { hasText: "Active" }).locator("+ dd")).toHaveText("Active");
  await expect(page.locator(".layout-panels > details > summary h2")).toHaveText(["General", "Lifecycle", "Record"]);
});

test("validity: lists hide inactive CIs by default and say when an active one deactivates", async ({ page, request }) => {
  scheduledId = (
    await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId, validUntil: iso(30 * DAY), attributes: { code: `core-${stamp}-scheduled` } })
  ).id;
  expiredId = (
    await apiSend<{ id: string }>(request, "POST", "/configuration-items", {
      classId,
      validFrom: iso(-2 * DAY),
      validUntil: iso(-DAY),
      attributes: { code: `core-${stamp}-expired` },
    })
  ).id;

  await page.goto(`/cis?classId=${classId}`);
  const scheduledRow = page.getByRole("row", { name: new RegExp(`core-${stamp}-scheduled`) });
  await expect(scheduledRow).toContainText(/Active\s*· deactivates on/);
  await expect(page.getByRole("link", { name: `core-${stamp}-expired` })).toHaveCount(0);

  await page.locator("#f-active").selectOption({ label: "Show inactive" });
  await expect(page).toHaveURL(/active=all/);
  await expect(page.getByRole("row", { name: new RegExp(`core-${stamp}-expired`) })).toContainText("Inactive");
  await page.reload();
  await expect(page.locator("#f-active")).toHaveValue("all");
  await expect(page.getByRole("link", { name: `core-${stamp}-expired` })).toBeVisible();
  await snap(page, "core-fields-list-inactive");

  await page.goto(`/cis/${scheduledId}`);
  await expect(page.locator(".page-header .badge.warn")).toHaveText(/Deactivates on/);
  const active = page.locator(".layout-panels dt", { hasText: "Active" }).locator("+ dd");
  await expect(active).toHaveText(/^Active\s*· deactivates on/);

  // Valid until may not precede valid from: the API's error lands next to the field.
  await page.getByRole("link", { name: "Edit", exact: true }).click();
  await page.locator("#f-valid-until").fill("2020-01-01T00:00");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.locator("#f-valid-until")).toHaveAttribute("aria-invalid", "true");
  await expect(page.locator("#f-valid-until-err")).toBeVisible();
  // A validity period in the past deactivates the CI at once.
  await page.locator("#f-valid-from").fill("2019-01-01T00:00");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${scheduledId}$`));
  await expect(page.locator(".page-header .badge.off")).toHaveText("Inactive");
  // Emptying valid until makes the CI open-ended and active again.
  await page.getByRole("link", { name: "Edit", exact: true }).click();
  await page.locator("#f-valid-until").fill("");
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  await expect(page.locator(".page-header .badge.off")).toHaveCount(0);
  await expect(active).toHaveText("Active");
});

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

test("ident: read-only for anyone but an administrator", async ({ page, request, browser }) => {
  // The e2e user is an administrator: the ident is editable.
  await page.goto(`/cis/${expiredId}/edit`);
  await expect(page.locator("#f-ident")).toBeEnabled();

  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E core editors ${stamp}`,
    globalPermissions: [],
    classPermissions: [{ classId, view: true, create: true, edit: true, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: USERNAME, email: `${USERNAME}@example.test`, displayName: `E2E Core ${stamp}`, password: PASSWORD, profileIds: [profile.id] });
  const editor = await signInUi(browser, USERNAME, PASSWORD);
  try {
    await editor.goto(`/cis/${expiredId}/edit`);
    await expect(editor.locator("#attr-code")).toBeEnabled();
    await expect(editor.locator("#f-ident")).toBeDisabled();
    await expect(editor.locator("#f-ident-hint")).toContainText("read-only");
    await editor.goto(`/cis/new?classId=${classId}`);
    await expect(editor.locator("#f-ident")).toBeDisabled();
    await expect(editor.locator("#f-ident")).toHaveAttribute("placeholder", "Generated");
    await expect(editor.locator("#f-valid-from")).toBeEnabled();
  } finally {
    await editor.context().close();
  }
});
