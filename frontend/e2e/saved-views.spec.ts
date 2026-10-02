import type { APIRequestContext, Browser, Page } from "@playwright/test";
import { apiGet, apiSend, checkA11y, classIdByName, csrf, expect, lookupValueId, snap, test } from "./support";

// Saved views on the inventory and the search page (saved-views spec §5.3): save, reload and bookmark,
// Modified and Revert, rename and delete, defaults (the list is queried once), shared views as a limited
// user, degraded and unavailable views, search views, the error states, the keyboard, and axe.
// Every view, attribute and lookup list here is named with this run's stamp, so the spec can share a database.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const OS_KEY = `e2e_sv_os_${stamp}`;
const PROFILE = `E2E view readers ${stamp}`;
const READER = `e2e-view-reader-${stamp}`;
const READER_PASSWORD = "view-reader-password-123";

interface View {
  id: string;
  name: string;
  version: number;
  isDefault: boolean;
  resolved: { state: string };
}

let serverId: string;
let production: string;

/** The Modified marker next to the View menu (text, not only colour). */
const modifiedMarker = (page: Page) => page.locator(".view-modified", { hasText: "Modified" });
const viewButton = (page: Page) => page.getByRole("button", { name: /^View / });
const menu = (page: Page) => page.getByRole("menu", { name: "Views" });
const menuItem = (page: Page, name: string) => menu(page).getByRole("menuitem", { name, exact: true });
const viewItem = (page: Page, name: string) => menu(page).getByRole("menuitemradio", { name: new RegExp(`^${name}\\b`) });

async function openMenu(page: Page) {
  await viewButton(page).click();
  await expect(menu(page)).toBeVisible();
}

async function views(request: APIRequestContext, context: "inventory" | "search" = "inventory"): Promise<View[]> {
  return (await apiGet<{ data: View[] }>(request, `/saved-views?context=${context}`)).data;
}
async function viewByName(request: APIRequestContext, name: string, context: "inventory" | "search" = "inventory"): Promise<View> {
  const v = (await views(request, context)).find((x) => x.name === name);
  expect(v, `view ${name}`).toBeTruthy();
  return v!;
}
async function createView(request: APIRequestContext, name: string, definition: object, visibility = "personal", context = "inventory"): Promise<View> {
  return apiSend<View>(request, "POST", "/saved-views", { context, name, visibility, definition });
}

/** The rows' labels, to compare what two loads of a view show. */
const labels = (page: Page) => page.locator("table.data tbody tr td:first-child").allInnerTexts();

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

test.beforeAll(async ({ request }) => {
  serverId = await classIdByName(request, "Server");
  production = await lookupValueId(request, "environment", "production");
  // A text attribute of Server for an attribute column and sort; archived in the degraded-view test.
  await apiSend(request, "POST", "/attribute-definitions", { classId: serverId, key: OS_KEY, label: `OS ${stamp}`, dataType: "text" });
});

test("save as new view, reload, and open the view= bookmark", async ({ page, browser, request }) => {
  const name = `E2E prod servers ${stamp}`;
  await page.goto(`/cis?classId=${serverId}&lookupValueId=${production}&sort=-label&columns=label,ident,attributes.${OS_KEY}`);
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await expect(viewButton(page)).toContainText("Unsaved view");
  const before = await labels(page);

  await openMenu(page);
  await menuItem(page, "Save as new view…").click();
  const dialog = page.getByRole("dialog", { name: "Save as new view" });
  await expect(dialog.getByLabel("Name")).toBeFocused();
  await dialog.getByLabel("Name").fill(name);
  await dialog.getByLabel("Description").fill("Production servers by label, newest first");
  await dialog.getByRole("button", { name: "Save view" }).click();
  await expect(dialog).toBeHidden();
  await expect(page).toHaveURL(/[?&]view=/);
  await expect(viewButton(page)).toContainText(name);
  await expect(modifiedMarker(page)).toHaveCount(0);
  await expect(page.getByRole("status").filter({ hasText: `View ${name} saved` })).toBeAttached();

  const v = await viewByName(request, name);
  // Reload: the URL alone restores the rows, the columns and the sort.
  await page.reload();
  await expect(viewButton(page)).toContainText(name);
  await expect(page.getByRole("columnheader", { name: `OS ${stamp}` })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: /^Label/ })).toHaveAttribute("aria-sort", "descending");
  expect(await labels(page)).toEqual(before);

  // A link with nothing but view=<id>, in a new page, shows the same.
  const other = await browser.newPage();
  await other.goto(`/cis?view=${v.id}`);
  await expect(other).toHaveURL(new RegExp(`sort=-label`));
  await expect(viewButton(other)).toContainText(name);
  await expect(other.getByRole("columnheader", { name: `OS ${stamp}` })).toBeVisible();
  expect(await labels(other)).toEqual(before);
  await other.close();
  await snap(page, "saved-view-applied");
});

test("Modified, Revert and Save", async ({ page, request }) => {
  const name = `E2E prod servers ${stamp}`;
  const v = await viewByName(request, name);
  await page.goto(`/cis?view=${v.id}`);
  await expect(viewButton(page)).toContainText(name);

  await page.getByRole("columnheader", { name: /^Label/ }).getByRole("button").click();
  await expect(modifiedMarker(page)).toBeVisible();
  await page.getByRole("button", { name: "Revert" }).click();
  await expect(modifiedMarker(page)).toHaveCount(0);
  await expect(page.getByRole("columnheader", { name: /^Label/ })).toHaveAttribute("aria-sort", "descending");

  await page.getByRole("columnheader", { name: /^Label/ }).getByRole("button").click();
  await expect(modifiedMarker(page)).toBeVisible();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(modifiedMarker(page)).toHaveCount(0);
  const saved = await viewByName(request, name);
  expect(saved.version).toBe(v.version + 1);
  await expect(page.getByRole("columnheader", { name: /^Label/ })).toHaveAttribute("aria-sort", "ascending");
});

test("a default view is applied from the menu and the list is queried once", async ({ page, request }) => {
  const name = `E2E prod servers ${stamp}`;
  const v = await viewByName(request, name);
  await page.goto(`/cis?view=${v.id}`);
  await openMenu(page);
  await menuItem(page, "Set as my default for Server").click();
  await expect.poll(async () => (await viewByName(request, name)).isDefault).toBe(true);
  await expect(viewButton(page).getByText("Default")).toBeVisible();

  // From the dashboard, open the Server list from the navigation: the default comes with it, in one list request.
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  const listRequests: string[] = [];
  page.on("request", (r) => {
    const u = new URL(r.url());
    if (u.pathname === "/api/v1/configuration-items" && u.searchParams.get("classId") === serverId) listRequests.push(u.search);
  });
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Server\b/ }).click();
  await expect(page).toHaveURL(new RegExp(`view=${v.id}`));
  await expect(viewButton(page)).toContainText(name);
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await page.waitForLoadState("networkidle");
  expect(listRequests, listRequests.join("\n")).toHaveLength(1);
  expect(listRequests[0]).toContain(`lookupValueId=${production}`);
});

test("rename to a taken name shows the error at the field; delete names the default it removes", async ({ page, request }) => {
  const name = `E2E prod servers ${stamp}`;
  const taken = `E2E taken ${stamp}`;
  await createView(request, taken, { classKeys: ["server"] });
  const v = await viewByName(request, name);
  await page.goto(`/cis?view=${v.id}`);

  await openMenu(page);
  await menuItem(page, "Rename…").click();
  const rename = page.getByRole("dialog", { name: "Rename view" });
  await rename.getByLabel("Name").fill(taken.toUpperCase());
  await rename.getByRole("button", { name: "Rename" }).click();
  const field = rename.getByLabel("Name");
  await expect(field).toHaveAttribute("aria-invalid", "true");
  await expect(field).toBeFocused();
  await expect(rename.locator("#sv-name-err")).toBeVisible();
  await rename.getByRole("button", { name: "Cancel" }).click();

  await openMenu(page);
  await menuItem(page, "Delete…").click();
  const del = page.getByRole("dialog", { name: `Delete view “${name}”?` });
  await expect(del.getByRole("button", { name: "Cancel" })).toBeFocused();
  await expect(del).toContainText("It is your default for Server");
  await del.getByRole("button", { name: "Delete view" }).click();
  await expect(del).toBeHidden();
  // The Server list as it opens without the view.
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page).not.toHaveURL(/view=/);
  await expect(viewButton(page)).toContainText("Unsaved view");
  expect((await views(request)).some((x) => x.id === v.id)).toBe(false);
});

test("share a copy; a reader without views.share copies it to their views", async ({ page, browser, request }) => {
  const personal = await createView(request, `E2E mine ${stamp}`, { classKeys: ["server"], sort: { field: "ident", direction: "asc" } });
  const sharedName = `E2E shared ${stamp}`;
  await page.goto(`/cis?view=${personal.id}`);
  await openMenu(page);
  await menuItem(page, "Share a copy…").click();
  const share = page.getByRole("dialog", { name: "Share a copy with everyone" });
  await share.getByLabel("Name").fill(sharedName);
  await share.getByRole("button", { name: "Share copy" }).click();
  await expect(share).toBeHidden();
  await openMenu(page);
  await expect(menu(page).getByRole("group", { name: "Shared views" }).getByRole("menuitemradio", { name: new RegExp(sharedName) })).toBeVisible();
  await page.keyboard.press("Escape");

  // A reader who may view Servers and holds no global right.
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: PROFILE,
    globalPermissions: [],
    classPermissions: [{ classId: serverId, view: true, create: false, edit: false, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: READER, displayName: `E2E Reader ${stamp}`, password: READER_PASSWORD, profileIds: [profile.id] });
  const reader = await signInUi(browser, READER, READER_PASSWORD);
  await reader.goto("/cis");
  await openMenu(reader);
  await viewItem(reader, sharedName).click();
  await expect(viewButton(reader)).toContainText(sharedName);
  await openMenu(reader);
  await expect(menuItem(reader, "Delete…")).toHaveCount(0);
  await expect(menuItem(reader, "Rename…")).toHaveCount(0);
  await expect(menuItem(reader, "Save view")).toHaveCount(0);
  await expect(menuItem(reader, "Share a copy…")).toHaveCount(0);
  await menuItem(reader, "Copy to my views").click();
  const copy = reader.getByRole("dialog", { name: "Copy to my views" });
  await expect(copy.getByLabel("Name")).toHaveValue(sharedName);
  await copy.getByRole("button", { name: "Copy view" }).click();
  await expect(copy).toBeHidden();
  await openMenu(reader);
  await expect(menu(reader).getByRole("group", { name: "My views" }).getByRole("menuitemradio", { name: new RegExp(sharedName) })).toBeVisible();
  await expect(menuItem(reader, "Delete…")).toBeVisible();
  await reader.context().close();
});

test("a degraded view says what was dropped; an unavailable one is never applied", async ({ page, request }) => {
  // Degraded: an attribute column archived after saving.
  const tmpKey = `e2e_sv_tmp_${stamp}`;
  const attr = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: serverId, key: tmpKey, label: `Tmp ${stamp}`, dataType: "text" });
  const degraded = await createView(request, `E2E degraded ${stamp}`, { classKeys: ["server"], columns: ["label", `attributes.${tmpKey}`] });
  await apiSend(request, "PATCH", `/attribute-definitions/${attr.id}`, { isActive: false });
  await page.goto(`/cis?view=${degraded.id}`);
  await expect(viewButton(page)).toContainText(`E2E degraded ${stamp}`);
  await expect(page.getByText("Parts of this view no longer exist and were left out")).toBeVisible();
  await expect(page.getByRole("columnheader", { name: `Tmp ${stamp}` })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Save to fix" })).toBeVisible();

  // Unavailable: every value of its lookup filter archived.
  const list = await apiSend<{ id: string }>(request, "POST", "/lookup-lists", { key: `e2e_sv_list_${stamp}`, name: `E2E SV list ${stamp}` });
  const values = [
    await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: list.id, key: "one", name: "One" }),
    await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: list.id, key: "two", name: "Two" }),
  ];
  const gone = await createView(request, `E2E gone ${stamp}`, { classKeys: ["server"], filters: { lookups: { [`e2e_sv_list_${stamp}`]: ["one", "two"] } } });
  for (const v of values) await apiSend(request, "PATCH", `/lookup-list-values/${v.id}`, { isActive: false });
  expect((await viewByName(request, `E2E gone ${stamp}`)).resolved.state).toBe("unavailable");

  await page.goto("/cis");
  const urlBefore = page.url();
  await openMenu(page);
  const item = viewItem(page, `E2E gone ${stamp}`);
  await expect(item).toHaveAttribute("aria-disabled", "true");
  // Disabled items stay focusable (menu pattern); Enter on it does nothing.
  await item.focus();
  await page.keyboard.press("Enter");
  await expect(menu(page)).toBeVisible();
  expect(page.url()).toBe(urlBefore);
  await page.keyboard.press("Escape");

  // Opened by link: not applied, and the page says so.
  await page.goto(`/cis?view=${gone.id}`);
  await expect(page.getByText(`The view “E2E gone ${stamp}” refers to a filter that no longer exists`)).toBeVisible();
  await expect(page).not.toHaveURL(/view=/);
});

test("a search view keeps the term and the filters, and cannot be a default", async ({ page }) => {
  const name = `E2E search ${stamp}`;
  await page.goto("/search?q=fra1");
  await expect(page.getByRole("heading", { level: 1 })).toContainText("fra1");
  await page.locator("#s-class").selectOption(serverId);
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await openMenu(page);
  await expect(menu(page).getByRole("menuitem", { name: /default/i })).toHaveCount(0);
  await menuItem(page, "Save as new view…").click();
  const dialog = page.getByRole("dialog", { name: "Save as new view" });
  await dialog.getByLabel("Name").fill(name);
  await dialog.getByRole("button", { name: "Save view" }).click();
  await expect(dialog).toBeHidden();

  await page.goto("/search?q=crm");
  await openMenu(page);
  await viewItem(page, name).click();
  await expect(page).toHaveURL(/q=fra1/);
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page.locator("#s-class")).toHaveValue(serverId);
});

test("error states: views that fail to load, a link to a missing view, a concurrent edit", async ({ page, request }) => {
  await page.route("**/api/v1/saved-views?*", (route) =>
    route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: { code: "INTERNAL", message: "boom", requestId: "req-e2e-sv" } }) }),
  );
  await page.goto(`/cis?classId=${serverId}`);
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await openMenu(page);
  await expect(page.getByText("Saved views could not be loaded.")).toBeVisible();
  await expect(page.getByText("req-e2e-sv")).toBeVisible();
  await page.unroute("**/api/v1/saved-views?*");
  await menuItem(page, "Retry").click();
  await expect(page.getByText("Saved views could not be loaded.")).toHaveCount(0);
  await expect(menuItem(page, "Save as new view…")).toBeVisible();
  await page.keyboard.press("Escape");

  await page.goto("/cis?view=00000000-0000-4000-8000-000000000000");
  await expect(page.getByText("The saved view in this link is not available to you. Showing the default list.")).toBeVisible();
  await expect(page).not.toHaveURL(/view=/);

  // Someone else saves the view while it is open here.
  const v = await createView(request, `E2E conflict ${stamp}`, { classKeys: ["server"] });
  await page.goto(`/cis?view=${v.id}`);
  await expect(viewButton(page)).toContainText(`E2E conflict ${stamp}`);
  const token = await csrf(request);
  const res = await request.patch(`/api/v1/saved-views/${v.id}`, { data: { version: v.version, description: "changed elsewhere" }, headers: { "X-CSRF-Token": token } });
  expect(res.ok()).toBeTruthy();
  await page.getByRole("columnheader", { name: /^Label/ }).getByRole("button").click();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  const conflict = page.getByRole("dialog", { name: "This view was changed elsewhere" });
  await expect(conflict).toBeVisible();
  await expect(conflict).toContainText("The saved view was changed by");
  await conflict.getByRole("button", { name: "Load latest" }).click();
  await expect(conflict).toBeHidden();
  await expect(modifiedMarker(page)).toHaveCount(0);
});

test("the View menu and its dialogs work with the keyboard alone", async ({ page, request }) => {
  const name = `E2E keys ${stamp}`;
  await createView(request, `E2E kb target ${stamp}`, { classKeys: ["server"], sort: { field: "ident", direction: "desc" } });
  await page.goto(`/cis?classId=${serverId}`);
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await viewButton(page).focus();
  await page.keyboard.press("Enter");
  await expect(menu(page)).toBeVisible();
  const focusedText = () => page.evaluate(() => document.activeElement?.textContent?.trim() ?? "");
  await expect.poll(() => page.evaluate(() => !!document.activeElement?.closest(".view-menu-popup"))).toBe(true);
  // With more than ten views the menu starts with a filter box: type to narrow, ↓ into the items.
  const filter = page.getByRole("searchbox", { name: "Filter views" });
  if (await filter.isVisible()) {
    await expect(filter).toBeFocused();
    await page.keyboard.type(`E2E kb target ${stamp}`);
    await page.keyboard.press("ArrowDown");
  }

  // Arrow to the view and apply it with Enter.
  for (let i = 0; i < 60 && !(await focusedText()).includes(`E2E kb target ${stamp}`); i++) await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(viewButton(page)).toContainText(`E2E kb target ${stamp}`);
  await expect(viewButton(page)).toBeFocused();

  // Esc closes the menu and returns focus to the button.
  await page.keyboard.press("Enter");
  await expect(menu(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu(page)).toBeHidden();
  await expect(viewButton(page)).toBeFocused();

  // Save as, by keyboard.
  await page.keyboard.press("ArrowUp"); // opens on the last item
  for (let i = 0; i < 20 && (await focusedText()) !== "Save as new view…"; i++) await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Save as new view" });
  await expect(dialog.getByLabel("Name")).toBeFocused();
  await page.keyboard.type(name);
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  await expect(viewButton(page)).toContainText(name);

  // Delete, by keyboard: focus starts on Cancel.
  await viewButton(page).focus();
  await page.keyboard.press("ArrowUp");
  for (let i = 0; i < 20 && (await focusedText()) !== "Delete…"; i++) await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Enter");
  const del = page.getByRole("dialog", { name: `Delete view “${name}”?` });
  await expect(del.getByRole("button", { name: "Cancel" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(del.getByRole("button", { name: "Delete view" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(del).toBeHidden();
  expect((await views(request)).some((x) => x.name === name)).toBe(false);
});

test("axe: the open View menu and every dialog, light and dark", async ({ page, request }, testInfo) => {
  const v = await createView(request, `E2E axe ${stamp}`, { classKeys: ["server"] });
  await page.goto(`/cis?view=${v.id}`);
  await expect(viewButton(page)).toContainText(`E2E axe ${stamp}`);
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await openMenu(page);
    await checkA11y(page, testInfo, `view-menu-${colorScheme}`);
    for (const [item, dialogName] of [
      ["Save as new view…", "Save as new view"],
      ["Rename…", "Rename view"],
      ["Delete…", `Delete view “E2E axe ${stamp}”?`],
      ["Manage views…", "Manage views"],
    ] as const) {
      if (!(await menu(page).isVisible())) await openMenu(page);
      await menuItem(page, item).click();
      const dialog = page.getByRole("dialog", { name: dialogName });
      await expect(dialog).toBeVisible();
      if (item === "Manage views…") await expect(dialog.getByRole("table").first()).toBeVisible();
      await checkA11y(page, testInfo, `view-dialog-${item.replace(/\W+/g, "-")}-${colorScheme}`);
      await page.keyboard.press("Escape");
      await expect(dialog).toBeHidden();
    }
  }
});
