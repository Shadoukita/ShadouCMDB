import type { APIRequestContext, Browser, Page } from "@playwright/test";
import { apiGet, apiSend, classIdByName, createCi, csrf, expect, lookupValueId, snap, test } from "./support";

// Saved views (SHAA-578 spec §5.3): the View menu of /cis and /search, the URL as the source of truth,
// defaults, shared views, degraded and unavailable views, error states and keyboard use. Runs as the
// e2e administrator on the demo seed's Server class with its own CIs ("zz-…": after the demo servers,
// which other specs open as "the first server by label"), and deletes every view it saved.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PREFIX = `zz-e2e-views-${stamp}`;
const vname = (n: string) => `${PREFIX} ${n}`;

interface SavedView {
  id: string;
  name: string;
  version: number;
  visibility: "personal" | "shared";
  isDefault: boolean;
  home: string | null;
  definition: { sort?: { field: string; direction: string }; columns?: string[]; filters?: { q?: string } };
  resolved: { state: string };
}

let serverId = "";
let prodId = "";
const cis: { id: string }[] = [];

const viewButton = (page: Page) => page.getByRole("button", { name: /^View:/ });
const menu = (page: Page) => page.getByRole("menu", { name: "Saved views" });
const modified = (page: Page) => page.locator(".view-modified");
const rows = (page: Page) => page.locator("table.data tbody tr");

async function openMenu(page: Page) {
  await viewButton(page).click();
  await expect(menu(page)).toBeVisible();
}
async function menuAction(page: Page, name: string | RegExp) {
  await openMenu(page);
  await menu(page).getByRole("menuitem", { name, exact: typeof name === "string" }).click();
}
const viewItem = (page: Page, name: string) => menu(page).getByRole("menuitemradio", { name: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`) });

/** The table as an operator sees it: the header texts and the labels in order. */
async function tableState(page: Page) {
  await page.waitForLoadState("networkidle");
  await expect(rows(page).first()).toBeVisible();
  await expect(page.locator("table.data.loading")).toHaveCount(0);
  return { headers: await page.locator("table.data thead th").allInnerTexts(), labels: await rows(page).locator("td:first-child").allInnerTexts() };
}

async function views(request: APIRequestContext, context: "inventory" | "search" = "inventory") {
  return (await apiGet<{ data: SavedView[] }>(request, `/saved-views?context=${context}`)).data;
}
async function viewByName(request: APIRequestContext, name: string, context: "inventory" | "search" = "inventory") {
  const v = (await views(request, context)).find((x) => x.name === name);
  expect(v, `view ${name}`).toBeTruthy();
  return v!;
}
async function createView(request: APIRequestContext, name: string, definition: object, extra: object = {}) {
  return apiSend<SavedView>(request, "POST", "/saved-views", { context: "inventory", name, visibility: "personal", definition, ...extra });
}
async function deleteView(request: APIRequestContext, v: { id: string; version: number }) {
  const res = await request.delete(`/api/v1/saved-views/${v.id}?version=${v.version}`, { headers: { "X-CSRF-Token": await csrf(request) } });
  expect([204, 404], `DELETE view → ${res.status()}`).toContain(res.status());
}
/** Every view this run (or an aborted earlier one) left behind: the menu turns into a filter list above 10. */
async function deleteE2eViews(request: APIRequestContext) {
  for (const context of ["inventory", "search"] as const) {
    for (const v of await views(request, context)) if (v.name.startsWith("zz-e2e-views-") && v.visibility === "personal") await deleteView(request, v);
    for (const v of await views(request, context)) if (v.name.startsWith(PREFIX)) await deleteView(request, v);
  }
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

/** The inventory's list requests (the sidebar's counts ask for limit=1). */
function listRequests(page: Page): URL[] {
  const seen: URL[] = [];
  page.on("request", (r) => {
    const url = new URL(r.url());
    if (r.method() === "GET" && url.pathname === "/api/v1/configuration-items" && url.searchParams.get("limit") !== "1") seen.push(url);
  });
  return seen;
}

test.beforeAll(async ({ request }) => {
  await deleteE2eViews(request);
  serverId = await classIdByName(request, "Server");
  prodId = await lookupValueId(request, "environment", "production");
  cis.push(await createCi(request, serverId, `${PREFIX}-a`, { hostname: "c-host", environment: prodId }));
  cis.push(await createCi(request, serverId, `${PREFIX}-b`, { hostname: "a-host", environment: prodId }));
  cis.push(await createCi(request, serverId, `${PREFIX}-c`, { hostname: "b-host", environment: prodId }));
  cis.push(await createCi(request, serverId, `${PREFIX}-d`, { hostname: "d-host" }));
});

test.afterAll(async ({ request }) => {
  await deleteE2eViews(request);
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const ci of cis) expect((await request.delete(`/api/v1/configuration-items/${ci.id}`, { headers })).status()).toBe(204);
});

const filteredUrl = () =>
  `/cis?classId=${serverId}&q=${encodeURIComponent(PREFIX)}&lookupValueId=${prodId}&sort=-attributes.hostname&columns=label,attributes.hostname,ident`;

test("1. save as new view; a reload and a view=-only link restore the rows, columns and sort", async ({ page, browser }) => {
  await page.goto(filteredUrl());
  await expect(viewButton(page)).toHaveText(/Unsaved view/);
  const before = await tableState(page);
  expect(before.labels).toEqual([`${PREFIX}-a`, `${PREFIX}-c`, `${PREFIX}-b`]); // production only, by hostname descending
  expect(before.headers.slice(0, 3)).toEqual(["Label", "Hostname ▼", "Ident"]);

  await menuAction(page, "Save as new view…");
  const dialog = page.getByRole("dialog", { name: "Save as new view" });
  await expect(dialog.getByLabel("Name", { exact: true })).toBeFocused();
  await dialog.getByLabel("Name", { exact: true }).fill(vname("Prod servers"));
  await dialog.getByLabel(/Description/).fill("Production servers by hostname");
  await dialog.getByRole("button", { name: "Save view" }).click();
  await expect(dialog).toBeHidden();
  await expect(page).toHaveURL(/[?&]view=/);
  await expect(viewButton(page)).toContainText(vname("Prod servers"));
  await expect(page.getByRole("status").filter({ hasText: `View ${vname("Prod servers")} saved` })).toBeAttached();
  const id = new URL(page.url()).searchParams.get("view")!;

  await page.reload();
  expect(await tableState(page)).toEqual(before);

  // A bookmark of the view: only view=<id>, in a fresh page.
  const other = await page.context().newPage();
  await other.goto(`/cis?view=${id}`);
  await expect(other).toHaveURL(/sort=-attributes\.hostname/);
  expect(await tableState(other)).toEqual(before);
  await expect(viewButton(other)).toContainText(vname("Prod servers"));
  await other.close();
  await snap(page, "saved-view-applied");
});

test("2. a change shows Modified; Revert restores the view, Save stores it with a new version", async ({ page, request }) => {
  const v = await viewByName(request, vname("Prod servers"));
  await page.goto(`/cis?view=${v.id}`);
  await expect(viewButton(page)).toContainText(vname("Prod servers"));
  await expect(modified(page)).toHaveCount(0);

  await page.locator("table.data thead").getByRole("button", { name: /^Label/ }).click();
  await expect(modified(page)).toHaveText(/Modified/);
  await page.getByRole("button", { name: "Revert", exact: true }).click();
  await expect(page).toHaveURL(/sort=-attributes\.hostname/);
  await expect(modified(page)).toHaveCount(0);

  await page.locator("table.data thead").getByRole("button", { name: /^Label/ }).click();
  await expect(modified(page)).toBeVisible();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(modified(page)).toHaveCount(0);
  const saved = await apiGet<SavedView>(request, `/saved-views/${v.id}`);
  expect(saved.version).toBe(v.version + 1);
  expect(saved.definition.sort).toEqual({ field: "label", direction: "asc" });
});

test("3. rename to a duplicate is refused at the field; delete names the view and that it is the default", async ({ page, request }) => {
  const v = await viewByName(request, vname("Prod servers"));
  const other = await createView(request, vname("Other"), { classKeys: ["server"], filters: { q: PREFIX } });
  await page.goto(`/cis?view=${v.id}`);
  await menuAction(page, "Rename…");
  const rename = page.getByRole("dialog", { name: /^Rename view/ });
  const name = rename.getByLabel("Name", { exact: true });
  await expect(name).toBeFocused();
  await name.fill(vname("OTHER")); // names are unique ignoring case
  await rename.getByRole("button", { name: "Rename" }).click();
  await expect(name).toHaveAttribute("aria-invalid", "true");
  await expect(rename.locator(".error")).toHaveText(/already have a view/);
  await expect(name).toBeFocused();
  await rename.getByRole("button", { name: "Cancel" }).click();
  await expect(rename).toBeHidden();
  await expect(viewButton(page)).toBeFocused();

  // As the user's default for Server, the delete confirmation says so.
  await apiSend(request, "PUT", "/saved-views/defaults", { context: "inventory", classKey: "server", viewId: v.id });
  await page.reload();
  await menuAction(page, "Delete…");
  const del = page.getByRole("dialog", { name: `Delete view “${vname("Prod servers")}”?` });
  await expect(del).toContainText("It is your default for Server. The Server list will open with the standard columns and filters.");
  await expect(del.getByRole("button", { name: "Cancel" })).toBeFocused();
  await del.getByRole("button", { name: "Delete view" }).click();
  await expect(del).toBeHidden();
  await expect(page).not.toHaveURL(/[?&]view=/);
  await expect(viewButton(page)).toContainText("Unsaved view");

  // The default went with it: the Server list opens with its admin list view (here: the built-in one).
  await page.goto(`/cis?classId=${serverId}`);
  await expect(rows(page).first()).toBeVisible();
  await expect(page).not.toHaveURL(/[?&]view=/);
  await deleteView(request, other);
});

test("4. a default applies when the class list is opened from the navigation, with one list request", async ({ page, request }) => {
  const v = await createView(request, vname("Default"), {
    classKeys: ["server"],
    filters: { q: PREFIX },
    sort: { field: "attributes.hostname", direction: "asc" },
    columns: ["label", "attributes.hostname"],
  });
  await page.goto(`/cis?view=${v.id}`);
  await menuAction(page, "Set as my default for Server");
  await expect(page.getByRole("status").filter({ hasText: "is now your default for Server" })).toBeAttached();
  await openMenu(page);
  await expect(viewItem(page, vname("Default"))).toContainText("Default");
  await page.keyboard.press("Escape");

  await page.goto("/");
  const seen = listRequests(page);
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Server\b/ }).click();
  await expect(page).toHaveURL(new RegExp(`[?&]view=${v.id}`));
  await expect(rows(page)).toHaveCount(4);
  await page.waitForLoadState("networkidle");
  const server = seen.filter((u) => u.searchParams.get("classId") === serverId);
  expect(
    server.map((u) => u.searchParams.get("sort")),
    "the list is queried once, with the default view's sort (GH#167)",
  ).toEqual(["attributes.hostname"]);
  expect(server[0].searchParams.get("q")).toBe(PREFIX);

  await menuAction(page, "Clear my default");
  await expect(page.getByRole("status").filter({ hasText: "Cleared your default for Server" })).toBeAttached();
  expect((await apiGet<SavedView>(request, `/saved-views/${v.id}`)).isDefault).toBe(false);
});

test.describe("5. shared views", () => {
  const PROFILE = `E2E views readers ${stamp}`;
  const USERNAME = `e2e-views-${stamp}`;
  const PASSWORD = `views-e2e-password-${stamp}`;

  test("an administrator shares a copy; a reader sees it without edit actions and copies it", async ({ page, request, browser }) => {
    const v = await viewByName(request, vname("Default"));
    await page.goto(`/cis?view=${v.id}`);
    await menuAction(page, "Share a copy…");
    const share = page.getByRole("dialog", { name: /^Share a copy of/ });
    await share.getByLabel("Name", { exact: true }).fill(vname("Shared"));
    await share.getByRole("button", { name: "Share copy" }).click();
    await expect(share).toBeHidden();
    const shared = await viewByName(request, vname("Shared"));
    expect(shared.visibility).toBe("shared");
    expect((await apiGet<SavedView>(request, `/saved-views/${v.id}`)).visibility).toBe("personal");

    const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
      name: PROFILE,
      globalPermissions: [],
      classPermissions: [{ classId: serverId, view: true, create: false, edit: false, delete: false }],
    });
    await apiSend(request, "POST", "/admin/users", { username: USERNAME, displayName: `E2E views ${stamp}`, password: PASSWORD, profileIds: [profile.id] });
    const reader = await signInUi(browser, USERNAME, PASSWORD);
    await reader.goto(`/cis?classId=${serverId}&q=${encodeURIComponent(PREFIX)}`);
    await openMenu(reader);
    const group = menu(reader).getByRole("group", { name: "Shared views" });
    await group.getByRole("menuitemradio", { name: new RegExp(vname("Shared")) }).click();
    await expect(viewButton(reader)).toContainText(vname("Shared"));
    await openMenu(reader);
    for (const action of ["Save view", "Rename…", "Delete…", "Share a copy…"]) await expect(menu(reader).getByRole("menuitem", { name: action, exact: true })).toHaveCount(0);
    await menu(reader).getByRole("menuitem", { name: "Copy to my views…" }).click();
    const copy = reader.getByRole("dialog", { name: /to my views$/ });
    await copy.getByLabel("Name", { exact: true }).fill(vname("Reader copy"));
    await copy.getByRole("button", { name: "Copy" }).click();
    await expect(viewButton(reader)).toContainText(vname("Reader copy"));
    await openMenu(reader);
    await expect(menu(reader).getByRole("group", { name: "My views" }).getByRole("menuitemradio", { name: new RegExp(vname("Reader copy")) })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await reader.context().close();
  });

  test("deleting a shared view says how many users have it as their default", async ({ page, request }) => {
    const shared = await viewByName(request, vname("Shared"));
    await page.goto(`/cis?view=${shared.id}`);
    await menuAction(page, "Delete…");
    const del = page.getByRole("dialog", { name: `Delete view “${vname("Shared")}”?` });
    await expect(del).toContainText("It is shared with everyone");
    await expect(del).toContainText("CIs are not affected");
    await del.getByRole("button", { name: "Delete view" }).click();
    await expect(del).toBeHidden();
  });
});

test("6. a degraded view says what was dropped; an unavailable one is disabled and never applied", async ({ page, request }) => {
  const cls = await apiSend<{ id: string; key: string }>(request, "POST", "/ci-classes", {
    key: `e2e_views_${stamp}`,
    name: `E2E views ${stamp}`,
    parentId: await classIdByName(request, "Hardware"),
  });
  const attr = await apiSend<{ id: string }>(request, "POST", "/attribute-definitions", { classId: cls.id, key: "os_version", label: "OS version", dataType: "text" });
  const list = await apiSend<{ id: string }>(request, "POST", "/lookup-lists", { key: `e2e_views_${stamp}`, name: `E2E views ${stamp}` });
  const a = await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: list.id, key: "a", name: "A" });
  const b = await apiSend<{ id: string }>(request, "POST", "/lookup-list-values", { listId: list.id, key: "b", name: "B" });
  const ci = await createCi(request, cls.id, `${PREFIX}-degraded`, { os_version: "12" });
  const degraded = await createView(request, vname("Degraded"), { classKeys: [cls.key], columns: ["label", "attributes.os_version"] });
  const gone = await createView(request, vname("Unavailable"), { classKeys: [cls.key], filters: { lookups: { [`e2e_views_${stamp}`]: ["a", "b"] } } });

  await apiSend(request, "PATCH", `/attribute-definitions/${attr.id}`, { isActive: false });
  await page.goto(`/cis?view=${degraded.id}`);
  await expect(page.locator(".view-notice")).toContainText(`Parts of the view “${vname("Degraded")}” no longer exist`);
  await expect(page.getByRole("button", { name: "Save to fix" })).toBeVisible();
  await expect(rows(page)).toHaveCount(1);
  await expect(page.locator("table.data thead")).not.toContainText("OS version");

  for (const v of [a, b]) await apiSend(request, "PATCH", `/lookup-list-values/${v.id}`, { isActive: false });
  const seen = listRequests(page);
  await page.goto(`/cis?classId=${serverId}&q=${encodeURIComponent(PREFIX)}`);
  await openMenu(page);
  const item = viewItem(page, vname("Unavailable"));
  await expect(item).toHaveAttribute("aria-disabled", "true");
  await expect(item).toHaveAttribute("title", "This view refers to a filter that no longer exists.");
  await item.click();
  await expect(page).not.toHaveURL(/[?&]view=/);
  await page.keyboard.press("Escape");
  // A link to it is not applied either: a banner, and the class is never listed without the filter.
  await page.goto(`/cis?view=${gone.id}`);
  await expect(page.locator(".view-notice")).toContainText("refers to a filter that no longer exists, so it was not applied");
  await page.waitForLoadState("networkidle");
  expect(seen.filter((u) => u.searchParams.get("classId") === cls.id)).toEqual([]);
  await snap(page, "saved-view-unavailable");
  await deleteView(request, await apiGet<SavedView>(request, `/saved-views/${degraded.id}`));
  await deleteView(request, await apiGet<SavedView>(request, `/saved-views/${gone.id}`));
  expect((await request.delete(`/api/v1/configuration-items/${ci.id}`, { headers: { "X-CSRF-Token": await csrf(request) } })).status()).toBe(204);
});

test("7. a search view restores the term and the filters; search views are never a default", async ({ page, request }) => {
  await page.goto(`/search?q=${encodeURIComponent(PREFIX)}`);
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await page.getByRole("group", { name: "Filter the results" }).getByLabel("Class").selectOption({ label: "Server" });
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await menuAction(page, "Save as new view…");
  const dialog = page.getByRole("dialog", { name: "Save as new view" });
  await expect(dialog).toContainText("Saves the search term and its filters.");
  await dialog.getByLabel("Name", { exact: true }).fill(vname("Search"));
  await dialog.getByRole("button", { name: "Save view" }).click();
  await expect(page).toHaveURL(/[?&]view=/);

  await page.goto("/search?q=something-else");
  await openMenu(page);
  await expect(menu(page).getByRole("menuitem", { name: /Set as my default/ })).toHaveCount(0);
  await viewItem(page, vname("Search")).click();
  await expect(page).toHaveURL(new RegExp(`q=${PREFIX}`));
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(`Results for “${PREFIX}”`);
  await openMenu(page);
  await expect(menu(page).getByRole("menuitem", { name: /Set as my default/ })).toHaveCount(0);
  await page.keyboard.press("Escape");
  expect((await viewByName(request, vname("Search"), "search")).home).toBeNull();
});

test.describe("8. errors", () => {
  test("a failed view list shows the error and Retry in the menu; the inventory still lists CIs", async ({ page }) => {
    await page.route("**/api/v1/saved-views?*", (route) =>
      route.fulfill({ status: 500, json: { error: { code: "INTERNAL_ERROR", message: "boom", requestId: "req-e2e-views" } } }),
    );
    await page.goto(`/cis?q=${encodeURIComponent(PREFIX)}`);
    await expect(rows(page)).toHaveCount(4);
    await viewButton(page).click();
    const popup = page.locator(".view-menu-popup");
    await expect(popup.getByRole("alert")).toContainText("Saved views could not be loaded.", { timeout: 20_000 });
    await expect(popup.getByRole("alert")).toContainText("req-e2e-views");
    await page.unroute("**/api/v1/saved-views?*");
    await menu(page).getByRole("menuitem", { name: "Retry" }).click();
    await expect(popup.getByRole("alert")).toHaveCount(0);
    await page.keyboard.press("Escape");
  });

  test("a link to a view that does not exist shows the not-available banner", async ({ page }) => {
    await page.goto("/cis?view=00000000-0000-4000-8000-000000000000");
    await expect(page.locator(".view-notice")).toHaveText(/The saved view in this link is not available to you\. Showing the default list\./);
    await expect(page).not.toHaveURL(/[?&]view=/);
    await expect(rows(page).first()).toBeVisible();
  });

  test("saving over a view changed elsewhere opens the conflict dialog", async ({ page, request }) => {
    const v = await viewByName(request, vname("Default"));
    await page.goto(`/cis?view=${v.id}`);
    await expect(viewButton(page)).toContainText(vname("Default"));
    await apiSend(request, "PATCH", `/saved-views/${v.id}`, { version: v.version, description: "changed elsewhere" });
    await page.locator("table.data thead").getByRole("button", { name: /^Label/ }).click();
    await page.getByRole("button", { name: "Save", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: /was changed elsewhere$/ });
    await expect(dialog).toContainText("This view was changed elsewhere");
    await expect(dialog.getByRole("button", { name: "Load latest" })).toBeFocused();
    await dialog.getByRole("button", { name: "Load latest" }).click();
    await expect(dialog).toBeHidden();
    await expect(page).toHaveURL(/sort=attributes\.hostname/);
    await expect(modified(page)).toHaveCount(0);
  });
});

test("9. keyboard only: open, move, apply, Esc; Save as and Delete", async ({ page, request }) => {
  await page.goto(`/cis?classId=${serverId}&q=${encodeURIComponent(PREFIX)}`);
  await expect(rows(page).first()).toBeVisible();
  const button = viewButton(page);
  await button.focus();
  await page.keyboard.press("Enter");
  await expect(menu(page).locator("[data-menu-item]").first()).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu(page)).toBeHidden();
  await expect(button).toBeFocused();

  // Save as: open with Enter, type "s" to jump to "Save as new view…", Enter, type the name, Enter.
  await page.keyboard.press("Enter");
  await page.keyboard.press("s");
  await expect(menu(page).getByRole("menuitem", { name: "Save as new view…" })).toBeFocused();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Save as new view" });
  await expect(dialog.getByLabel("Name", { exact: true })).toBeFocused();
  await page.keyboard.type(vname("Keyboard"));
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  await expect(button).toContainText(vname("Keyboard"));

  // Apply another view with the arrows: ↓ from the button opens on the first item.
  await page.goto(`/cis?classId=${serverId}&q=${encodeURIComponent(PREFIX)}&sort=ident`);
  await viewButton(page).focus();
  await page.keyboard.press("ArrowDown");
  const items = menu(page).getByRole("menuitemradio");
  const names = await items.allInnerTexts();
  const target = names.findIndex((n) => n.includes(vname("Keyboard")));
  for (let i = 0; i < target; i++) await page.keyboard.press("ArrowDown");
  await expect(items.nth(target)).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(viewButton(page)).toContainText(vname("Keyboard"));
  await expect(viewButton(page)).toBeFocused();

  // Delete: "d" jumps to "Delete…"; the confirmation starts on Cancel, Tab moves to Delete view.
  await page.keyboard.press("Enter");
  await page.keyboard.press("d");
  await expect(menu(page).getByRole("menuitem", { name: "Delete…" })).toBeFocused();
  await page.keyboard.press("Enter");
  const del = page.getByRole("dialog", { name: `Delete view “${vname("Keyboard")}”?` });
  await expect(del.getByRole("button", { name: "Cancel" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(del.getByRole("button", { name: "Delete view" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(del).toBeHidden();
  expect((await views(request)).some((v) => v.name === vname("Keyboard"))).toBe(false);
});

test("Manage views lists the user's views with their context and default", async ({ page }) => {
  await page.goto(`/cis?q=${encodeURIComponent(PREFIX)}`);
  await menuAction(page, "Manage views…");
  const dialog = page.getByRole("dialog", { name: "Manage views" });
  const mine = dialog.getByRole("region", { name: "My views" });
  await expect(mine.getByRole("row", { name: new RegExp(vname("Default")) })).toContainText("Inventory");
  await expect(mine.getByRole("row", { name: new RegExp(vname("Search")) })).toContainText("Search");
  await expect(dialog.getByRole("button", { name: "Close" })).toBeFocused();
  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(dialog).toBeHidden();
  await expect(viewButton(page)).toBeFocused();
});

