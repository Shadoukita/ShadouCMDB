import type { APIRequestContext, Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, classIdByName, createCi, csrf, expect, test } from "./support";

// Business services U2 (SHAA-933; spec SHAA-927 §5.1–5.4, §5.8, §5.10): the nav entry, the list with its URL
// filters, the detail with its tabs, the owners editor (combobox, order, conflict, per-token errors), the delete
// dialog and the error states. The members picker, the "Part of" panel and the groups admin are U3/U4.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "services-password-123";
const OUTSIDER = `e2e-svc-outsider-${stamp}`;
const ANN = `Ann Owner ${stamp}`;
const DORA = `Dora Leaver ${stamp}`;
const BEN = `Ben Second ${stamp}`;
const TEMP = `Tmp Gone ${stamp}`;
const SHOP = `Shop ${stamp}`;
const PAY = `Payments ${stamp}`;
const WEB = `svc-web-${stamp}`;

const ids: Record<string, string> = {};
let classId = "";

const user = async (request: APIRequestContext, username: string, displayName: string, profileIds: string[] = []) =>
  (await apiSend<{ id: string }>(request, "POST", "/admin/users", { username, email: `${username}@example.test`, displayName, password: PASSWORD, profileIds })).id;

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

async function send(request: APIRequestContext, method: string, path: string, data?: unknown) {
  const res = await request.fetch(`/api/v1${path}`, { method, data, headers: { "X-CSRF-Token": await csrf(request) } });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return res;
}

const service = (request: APIRequestContext, id: string) => apiGet<{ version: number; owners: { technical: { id: string }[]; business: { id: string }[] } }>(request, `/business-services/${id}`);

/** Picks a user or group in the owner combobox labelled `label` by typing `search` and pressing Enter on the first match. */
async function pickOwner(page: Page, label: string, search: string, name: string) {
  const box = page.getByRole("combobox", { name: label, exact: true });
  await box.fill(search);
  await expect(page.getByRole("option").filter({ hasText: name }).first()).toBeVisible();
  await expect(box).toHaveAttribute("aria-activedescendant", /.+/);
  // The result count is announced in the field's polite live region.
  await expect(box.locator("xpath=..").getByRole("status")).toHaveText(/found\./);
  await box.press("Enter");
}

test.beforeAll(async ({ request }) => {
  classId = (await apiGet<{ classId: string }>(request, "/settings/business-services")).classId;
  ids.ann = await user(request, `e2e-ann-${stamp}`, ANN);
  ids.dora = await user(request, `e2e-dora-${stamp}`, DORA);
  ids.ben = await user(request, `e2e-ben-${stamp}`, BEN);
  ids.temp = await user(request, `e2e-tmp-${stamp}`, TEMP);
  await apiSend(request, "PATCH", `/admin/users/${ids.dora}`, { isActive: false });
  // An account whose profile grants no class at all: no business services for them.
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: `E2E svc outsiders ${stamp}`, globalPermissions: [], classPermissions: [] });
  await user(request, OUTSIDER, OUTSIDER, [profile.id]);
  ids.pay = (await createCi(request, classId, PAY)).id;
  ids.web = (await createCi(request, await classIdByName(request, "Server"), WEB)).id;
  // Payments carries three technical owners, so its cell shows two names and "+1".
  const pay = await service(request, ids.pay);
  await apiSend(request, "PUT", `/business-services/${ids.pay}/owners`, {
    version: pay.version,
    technical: [ids.ann, ids.ben, ids.temp].map((id) => ({ kind: "user", id })),
    business: [],
  });
});

test("the nav entry leads to the list; headers sort with aria-sort, kept in the URL", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("link", { name: "Business services", exact: true }).click();
  await expect(page).toHaveURL(/\/services$/);
  await expect(page.getByRole("heading", { level: 1, name: "Business services" })).toBeVisible();
  const name = page.getByRole("columnheader", { name: /^Name/ });
  await expect(name).toHaveAttribute("aria-sort", "none");
  await expect(page.getByRole("columnheader", { name: /^Criticality/ })).toHaveAttribute("aria-sort", "ascending");
  await name.getByRole("button").click();
  await expect(page).toHaveURL(/sort=name/);
  await expect(name).toHaveAttribute("aria-sort", "ascending");
  await name.getByRole("button").click();
  await expect(name).toHaveAttribute("aria-sort", "descending");
  await page.reload();
  await expect(page.getByRole("columnheader", { name: /^Name/ })).toHaveAttribute("aria-sort", "descending");
});

test("owner cells show two names, then +N", async ({ page }) => {
  await page.goto(`/services?q=${encodeURIComponent(PAY)}`);
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: PAY }) });
  await expect(row.locator("td").nth(3)).toHaveText(new RegExp(`${ANN}, ${BEN}\\s*\\+1`));
  await expect(row.locator("td").nth(4)).toHaveText("None");
});

test("create: the operator lands on the detail with the Owners editor open, picks, orders and saves owners", async ({ page }, testInfo) => {
  await page.goto("/services");
  await page.getByRole("link", { name: "Create business service" }).first().click();
  await expect(page).toHaveURL(new RegExp(`/cis/new\\?classId=${classId}&return=services`));
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Business services");
  await page.locator("#attr-name").fill(SHOP);
  await page.locator("#attr-status").selectOption({ label: "In service" });
  await page.getByRole("button", { name: /^Create / }).click();

  await expect(page).toHaveURL(/\/services\/[0-9a-f-]{36}$/);
  ids.shop = page.url().split("/").pop()!;
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toContainText(`Business services${SHOP}`);
  const technical = page.getByRole("combobox", { name: "Add to Technical owners" });
  await expect(technical).toBeFocused();
  await expect(page.getByRole("note").first()).toHaveText(/No owner assigned\. Every business service should have a technical and a business owner\./);

  // One character is not a search; two are, 250 ms after the last keystroke.
  await technical.fill("A");
  await expect(page.getByText("Type at least 2 characters.").first()).toBeVisible();
  await expect(technical).toHaveAttribute("aria-expanded", "false");
  await pickOwner(page, "Add to Technical owners", `Ann Owner ${stamp}`, ANN);
  await pickOwner(page, "Add to Technical owners", `Ben Second ${stamp}`, BEN);
  // Ben moves up with the button, no drag.
  await page.getByRole("button", { name: `Move ${BEN} up` }).click();
  await expect(page.getByRole("button", { name: `Move ${BEN} down` })).toBeFocused();
  await pickOwner(page, "Add to Business owners", `Dora Leaver ${stamp}`, DORA);
  await expect(page.getByText(`${DORA} is disabled and will not be able to act as owner.`)).toBeVisible();
  await checkA11y(page, testInfo, "owners editor", { include: ".owners-card" });
  await page.getByRole("button", { name: "Save owners" }).click();

  const card = page.locator(".owners-card");
  await expect(card.getByRole("list", { name: "Technical owners" }).getByRole("listitem")).toHaveText([`${BEN} User`, `${ANN} User`]);
  await expect(card.getByRole("list", { name: "Business owners" }).getByRole("listitem")).toHaveText([`${DORA} (disabled) User`]);
  const saved = await service(page.request, ids.shop);
  expect(saved.owners.technical.map((o) => o.id)).toEqual([ids.ben, ids.ann]);
  await checkA11y(page, testInfo, "service overview");
});

test("the service opens ready to edit like any CI: Save once something changed, Discard drops it (GH#588)", async ({ page }) => {
  await page.goto(`/services/${ids.shop}`);
  // No separate edit mode: the Overview's values are inputs, and with nothing changed there is nothing to save.
  await expect(page.getByRole("link", { name: "Edit", exact: true })).toHaveCount(0);
  await expect(page.locator("#attr-name")).toHaveValue(SHOP);
  const bar = page.getByRole("region", { name: "Unsaved changes" });
  await expect(bar).toHaveCount(0);

  await page.locator("#attr-name").fill(`${SHOP} draft`);
  await expect(bar).toBeVisible();
  await bar.getByRole("button", { name: "Discard" }).click();
  await expect(bar).toHaveCount(0);
  await expect(page.locator("#attr-name")).toHaveValue(SHOP);

  // Save sends the change and the page stays as it is; the header follows.
  await page.locator("#attr-name").fill(`${SHOP} renamed`);
  await bar.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${SHOP} renamed.` })).toBeVisible();
  await expect(bar).toHaveCount(0);
  await expect(page).toHaveURL(new RegExp(`/services/${ids.shop}$`));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(`${SHOP} renamed`);
  await page.reload();
  await expect(page.locator("#attr-name")).toHaveValue(`${SHOP} renamed`);

  // Back to the name the later tests use.
  await page.locator("#attr-name").fill(SHOP);
  await bar.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(SHOP);
});

test("list filters live in the URL: reload and Back keep them; the disabled owner is marked in text", async ({ page }) => {
  await page.goto("/services");
  await page.locator("#svc-q").fill(stamp);
  await expect(page).toHaveURL(new RegExp(`q=${stamp}`));
  await page.getByLabel("Owner state").selectOption("disabled");
  await expect(page).toHaveURL(/ownerState=disabled/);
  const rows = page.locator("table.data tbody tr");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText(SHOP);
  await expect(rows.first()).toContainText(`${DORA} (disabled)`);
  await page.reload();
  await expect(page.getByLabel("Owner state")).toHaveValue("disabled");
  await expect(page.locator("#svc-q")).toHaveValue(stamp);
  await expect(rows).toHaveCount(1);
  await page.goBack();
  await expect(page.getByLabel("Owner state")).toHaveValue("");
  await expect(rows).toHaveCount(2);

  // The owner picker filters by a user; the role narrows it.
  await pickOwner(page, "Owner", `Ben Second ${stamp}`, BEN);
  await expect(page).toHaveURL(new RegExp(`owner=${ids.ben}`));
  await expect(rows).toHaveCount(2);
  await page.getByLabel("Owner role").selectOption("business");
  await expect(page.getByText("No business services match these filters.")).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).first().click();
  await expect(page).toHaveURL(/\/services$/);
});

test("409: someone else saved first; Reload shows their owners next to the kept selection", async ({ page, request }) => {
  await page.goto(`/services/${ids.shop}`);
  await page.getByRole("button", { name: "Edit owners" }).click();
  await page.getByRole("button", { name: `Remove ${ANN} as technical owner` }).click();
  const current = await service(request, ids.shop);
  await apiSend(request, "PUT", `/business-services/${ids.shop}/owners`, { version: current.version, technical: [{ kind: "user", id: ids.temp }], business: [] });
  await page.getByRole("button", { name: "Save owners" }).click();
  const alert = page.getByRole("alert").filter({ hasText: "This service was changed by someone else. Reload to see the current owners." });
  await expect(alert).toBeVisible();
  await expect(alert).toBeFocused();
  await alert.getByRole("button", { name: "Reload" }).click();
  await expect(alert.getByRole("heading", { name: "Current Technical owners" })).toBeVisible();
  await expect(alert).toContainText(TEMP);
  // The unsaved selection is still in the form; saving now replaces the reloaded owners with it.
  await expect(page.locator(".token-list").first()).toContainText(BEN);
  await page.getByRole("button", { name: "Save owners" }).click();
  await expect(page.locator(".owners-card").getByRole("list", { name: "Technical owners" })).toHaveText(new RegExp(BEN));
});

test("400 per token: an owner deleted meanwhile is marked on its token", async ({ page, request }) => {
  await page.goto(`/services/${ids.shop}`);
  await page.getByRole("button", { name: "Edit owners" }).click();
  await pickOwner(page, "Add to Business owners", `Tmp Gone ${stamp}`, TEMP);
  await send(request, "DELETE", `/admin/users/${ids.temp}`);
  await page.getByRole("button", { name: "Save owners" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "1 owner could not be saved" })).toBeVisible();
  const token = page.locator(".token.invalid");
  await expect(token).toContainText(TEMP);
  await expect(token).toContainText("This user or group no longer exists.");
  await page.getByRole("button", { name: `Remove ${TEMP} as business owner` }).click();
  await page.getByRole("button", { name: "Save owners" }).click();
  await expect(page.getByRole("button", { name: "Edit owners" })).toBeVisible();
});

test("a service's CI URL redirects; its Impact tab defaults to Upstream and a link's direction wins", async ({ page, request }) => {
  await apiSend(request, "POST", `/business-services/${ids.shop}/members`, { memberIds: [ids.web, ids.pay] });
  await page.goto(`/cis/${ids.shop}`);
  await expect(page).toHaveURL(new RegExp(`/services/${ids.shop}$`));
  await expect(page.getByRole("tab", { name: "Members (2)" })).toBeVisible();
  await page.getByRole("tab", { name: "Members (2)" }).click();
  await expect(page).toHaveURL(/tab=members/);
  await expect(page.getByRole("link", { name: WEB })).toHaveAttribute("href", `/cis/${ids.web}`);
  await expect(page.getByRole("link", { name: PAY })).toHaveAttribute("href", `/services/${ids.pay}`);
  await page.reload();
  await expect(page.getByRole("tab", { name: "Members (2)" })).toHaveAttribute("aria-selected", "true");

  await page.getByRole("tab", { name: "Impact" }).click();
  await expect(page).toHaveURL(new RegExp(`/services/${ids.shop}/impact$`));
  await expect(page.getByRole("radio", { name: /Upstream/ })).toBeChecked();
  await page.goto(`/cis/${ids.shop}/impact?direction=downstream`);
  await expect(page).toHaveURL(new RegExp(`/services/${ids.shop}/impact\\?direction=downstream$`));
  await expect(page.getByRole("radio", { name: /Downstream/ })).toBeChecked();
  for (const tab of ["Relationship map", "History", "Overview"]) {
    await page.getByRole("tab", { name: tab }).click();
    await expect(page.getByRole("tab", { name: tab })).toHaveAttribute("aria-selected", "true");
  }
});

test("delete: Confirm waits for the included-in check, and a failed check offers Retry (GH#477)", async ({ page }) => {
  const lookup = "**/api/v1/configuration-items/*/business-services*";
  let release!: () => void;
  const held = new Promise<void>((r) => (release = r));
  let fail = false;
  await page.route(lookup, async (route) => {
    await held;
    return fail
      ? route.fulfill({ status: 503, json: { error: { code: "SERVER_BUSY", message: "busy", requestId: "req-busy" } } })
      : route.fallback();
  });
  await page.goto(`/services/${ids.pay}`);
  await page.getByRole("button", { name: "Delete" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete business service ${PAY}?` });
  const confirm = dialog.getByRole("button", { name: "Delete business service" });
  await expect(dialog).toContainText("Checking which services include it");
  await expect(confirm).toBeDisabled();
  fail = true;
  release();
  // The query client retries a 5xx twice with backoff before it reports the error.
  await expect(dialog.getByText("The server is busy. Try again in a moment.")).toBeVisible({ timeout: 10_000 });
  await expect(confirm).toBeDisabled();
  fail = false;
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(dialog).toContainText("It is also part of 1 other business service; it is removed from it.");
  await expect(confirm).toBeEnabled();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await page.unroute(lookup);
});

test("delete: the dialog names the memberships and the services it is nested in", async ({ page }, testInfo) => {
  await page.goto(`/services/${ids.pay}`);
  await page.getByRole("button", { name: "Delete" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete business service ${PAY}?` });
  await expect(dialog).toContainText("Its 0 memberships are removed. The member CIs themselves are not deleted.");
  await expect(dialog).toContainText("It is also part of 1 other business service; it is removed from it.");
  await checkA11y(page, testInfo, "delete dialog", { include: "dialog[open]" });
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await page.goto(`/services/${ids.shop}`);
  await page.getByRole("button", { name: "Delete" }).click();
  const shop = page.getByRole("dialog", { name: `Delete business service ${SHOP}?` });
  await expect(shop).toContainText("Its 2 memberships are removed.");
  await expect(shop).not.toContainText("It is also part of");
  await shop.getByRole("button", { name: "Delete business service" }).click();
  await expect(page).toHaveURL(/\/services$/);
  await page.goto(`/services/${ids.shop}`);
  await expect(page.getByText("This business service does not exist or you do not have access to it.")).toBeVisible();
});

test("states: empty, busy with Retry, error panel with the request id", async ({ page }, testInfo) => {
  await page.goto(`/services?q=no-such-service-${stamp}`);
  await expect(page.getByText("No business services match these filters.")).toBeVisible();

  await page.route("**/api/v1/business-services?*", (route) =>
    route.fulfill({ status: 200, json: { data: [], page: { limit: 50, offset: 0, total: 0 }, visibility: "all_classes" } }),
  );
  await page.goto("/services");
  await expect(page.getByText(/^No business services yet\. A business service groups the CIs/)).toBeVisible();
  await expect(page.getByRole("link", { name: "Create business service" })).toHaveCount(2);
  await checkA11y(page, testInfo, "service list empty");
  await page.unroute("**/api/v1/business-services?*");

  let busy = true;
  await page.route("**/api/v1/business-services?*", (route) =>
    busy
      ? route.fulfill({ status: 503, json: { error: { code: "SERVER_BUSY", message: "busy", requestId: "req-busy" } } })
      : route.fallback(),
  );
  await page.goto("/services");
  await expect(page.getByText("The server is busy. Try again in a moment.")).toBeVisible();
  busy = false;
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.locator("table.data")).toBeVisible();
  await checkA11y(page, testInfo, "service list");
  await page.unroute("**/api/v1/business-services?*");

  await page.route("**/api/v1/business-services?*", (route) =>
    route.fulfill({ status: 500, json: { error: { code: "INTERNAL", message: "Something broke", requestId: "req-e2e-500" } } }),
  );
  await page.goto("/services");
  await expect(page.getByRole("alert")).toContainText("Something broke");
  await expect(page.getByRole("alert")).toContainText("req-e2e-500");
});

test("a user without view on the service class has no nav entry and gets the permission page", async ({ browser }) => {
  const page = await signInUi(browser, OUTSIDER);
  await expect(page.getByRole("link", { name: "Business services", exact: true })).toHaveCount(0);
  await page.goto("/services");
  await expect(page.getByText("You do not have permission to view business services.")).toBeVisible();
  await page.goto(`/services/${ids.pay}`);
  await expect(page.getByText("You do not have permission to view business services.")).toBeVisible();
  await page.context().close();
});
