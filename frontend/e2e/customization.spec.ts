import type { APIRequestContext, Page } from "@playwright/test";
import { apiGet, ciIdByName, classIdByName, createCi, csrf, expect, fieldLabels, resetUiSettings as resetSettings, saveCi, saveLayout, snap, test, chooseTheme } from "./support";

// Administration › Customization and Export / import, against the demo seed (the Server class
// and its attributes). The settings apply to every user, so the walk starts from and ends with
// the built-in settings (and no logo): the other specs see the stock UI.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const APP = `E2E CMDB ${stamp}`;
const PRIMARY = "#8a3ffc";
// 1×1 PNG.
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==", "base64");

interface Settings {
  version: number;
  settings: Record<string, unknown>;
}

async function save(page: Page, comment: string) {
  const bar = page.getByRole("region", { name: "Save changes" });
  await expect(bar.getByText("Unsaved changes")).toBeVisible();
  await bar.getByLabel("Comment for this version").fill(`${comment} ${stamp}`);
  await bar.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status").filter({ hasText: /Saved as version \d+/ })).toBeVisible();
  await expect(bar.getByText("No unsaved changes")).toBeVisible();
}

// Servers created for the sort walk. They are deleted afterwards: "e2e-sort-…" sorts before the demo
// servers, and later specs open the first Server by label and expect its demo relationships.
const sortCis: string[] = [];

test.beforeAll(async ({ request }) => resetSettings(request));
test.afterAll(async ({ request }) => {
  await resetSettings(request);
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of sortCis.splice(0)) {
    const res = await request.delete(`/api/v1/configuration-items/${id}`, { headers });
    expect(res.ok(), `delete ${id} → ${res.status()}`).toBeTruthy();
  }
});

test("the editor loads when Customization is opened again from within the app", async ({ page }) => {
  const adminNav = page.getByRole("navigation", { name: "Administration" });
  await page.goto("/admin");
  await adminNav.getByRole("link", { name: "Customization" }).click();
  await expect(page.getByLabel("Application name")).toBeVisible();
  // Back to Administration and in again without a reload: the settings are cached now, the editor must still appear.
  await page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: "Administration" }).click();
  // /admin opens its first page (Users).
  await expect(page).not.toHaveURL(/\/admin\/customization/);
  await adminNav.getByRole("link", { name: "Customization" }).click();
  await expect(page.getByLabel("Application name")).toBeVisible();
  await page.getByRole("navigation", { name: "Customization" }).getByRole("link", { name: "Navigation" }).click();
  await expect(page.getByRole("button", { name: "Add section" })).toBeVisible();
});

test("branding: name, colour, theme and logo apply app-wide and on the sign-in page", async ({ page, browser }) => {
  await page.goto("/admin");
  await page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Customization" }).click();
  await expect(page).toHaveURL(/\/admin\/customization\/branding$/);

  await page.getByLabel("Application name").fill(APP);
  // Live preview: the real header shows the draft before it is saved.
  await expect(page.locator(".shell-brand")).toContainText(APP);
  await page.getByLabel("Primary colour", { exact: true }).fill(PRIMARY);
  await page.getByLabel("Primary colour", { exact: true }).fill("not-a-colour");
  await expect(page.getByText("Use #rrggbb")).toBeVisible();
  await page.getByLabel("Primary colour", { exact: true }).fill(PRIMARY);
  await page.getByRole("radio", { name: "Dark" }).check();
  await save(page, "e2e branding");

  await page.getByLabel("Upload…").first().setInputFiles({ name: "logo.png", mimeType: "image/png", buffer: PNG });
  await expect(page.locator(".shell-brand img.brand-logo")).toBeVisible();
  await snap(page, "customization-branding");

  await page.goto("/");
  await expect(page.locator(".shell-brand")).toContainText(APP);
  await expect(page).toHaveTitle(`Dashboard · ${APP}`);
  const vars = await page.evaluate(() => ({
    theme: document.documentElement.dataset.theme,
    primary: getComputedStyle(document.documentElement).getPropertyValue("--c-primary").trim(),
  }));
  expect(vars).toEqual({ theme: "dark", primary: PRIMARY });

  // A user's own theme choice wins over the default, and survives a reload.
  await chooseTheme(page, "light");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await chooseTheme(page, "");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  const anon = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const login = await anon.newPage();
  await login.goto("/login");
  await expect(login.locator(".bare-brand")).toContainText(APP);
  await expect(login.locator(".bare-brand img.brand-logo")).toBeVisible();
  await expect(login).toHaveTitle(`Sign in · ${APP}`);
  await anon.close();
});

test("navigation: renamed entries, a section and a hidden page show in the main menu", async ({ page }) => {
  await page.goto("/admin/customization/navigation");
  const table = page.locator(".nav-editor table");
  await table.getByLabel("Name for All configuration items").fill("Inventory");
  await table.getByLabel("Name for All configuration items").blur();
  await table.getByLabel("Show Search").check();
  await page.getByLabel("New section").fill("Compute");
  await page.getByRole("button", { name: "Add section" }).click();
  await table.getByLabel("Move Server into a section").selectOption({ label: "Compute" });
  // The real menu previews the draft.
  const nav = page.getByRole("navigation", { name: "Main" });
  await expect(nav.getByRole("heading", { name: "Compute" })).toBeVisible();
  await save(page, "e2e navigation");
  await snap(page, "customization-navigation");

  await page.goto("/");
  await expect(nav.getByRole("link", { name: "Inventory", exact: true })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Search", exact: true })).toBeVisible();
  await expect(nav.getByRole("link", { name: "All configuration items" })).toHaveCount(0);
  const compute = nav.getByRole("heading", { name: "Compute" });
  await expect(compute).toBeVisible();
  await nav.getByRole("link", { name: /^Server/ }).click();
  await expect(page).toHaveURL(/\/cis\?classId=/);
});

test("dashboard: chosen widgets replace the built-in panels, with a saved search", async ({ page }) => {
  await page.goto("/admin/customization/dashboard");
  await page.getByRole("radio", { name: "Choose the widgets" }).check();
  await page.getByLabel("Add a widget").selectOption("saved_search");
  await page.getByRole("button", { name: "Add", exact: true }).click();
  const row = page.locator("tr", { has: page.locator("code", { hasText: /^saved_search$/ }) });
  await row.getByLabel("Title of saved_search").fill("Servers in service");
  await row.getByRole("group", { name: "Classes" }).getByLabel("Server", { exact: true }).check();
  // Lookup filters: one checklist per lookup list (the status is a lookup attribute).
  await row.getByRole("group", { name: "Status", exact: true }).getByLabel("In service").check();
  await page.getByLabel("Remove by_status").click();
  // The preview renders the draft with live data.
  await expect(page.getByLabel("Dashboard preview").getByRole("heading", { name: /Servers in service/ })).toBeVisible();
  await save(page, "e2e dashboard");

  await page.goto("/");
  const widget = page.locator('[data-widget="saved_search"]');
  await expect(widget.getByRole("heading", { name: /Servers in service/ })).toBeVisible();
  await expect(widget.getByRole("row").nth(1)).toContainText("Server");
  await expect(page.locator('[data-widget="by_status"]')).toHaveCount(0);
  await expect(page.locator('[data-widget="by_class"]')).toBeVisible();
  await snap(page, "customization-dashboard");
});

test("list views: a class's columns, default sort, filter and page size apply to its inventory", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  await page.goto("/admin/customization/list-views");
  await page.getByLabel("Class").selectOption("server");
  await expect(page).toHaveURL(/class=server/);
  await page.getByRole("button", { name: "Customize the Server list" }).click();
  await page.getByLabel("Remove Ident").click();
  await page.getByLabel("Add to Columns").selectOption({ label: "CPU cores (attribute)" });
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await page.getByLabel("Default sort").selectOption("updatedAt");
  await page.getByLabel("Sort direction").selectOption("desc");
  await page.getByLabel("Rows per page (10-200)").fill("25");
  await page.getByLabel("Rows per page (10-200)").blur();
  await page.getByRole("group", { name: "Default: Status" }).getByLabel("In service").check();
  await expect(page.getByLabel("List preview").getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await save(page, "e2e list view");

  // Opened from the menu, the list gets the default filter in its URL: it shows in the toolbar and can be changed.
  await page.goto("/");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Server/ }).click();
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page).toHaveURL(/lookupValueId=/);
  await expect(page.locator("form.toolbar")).toContainText("In service");
  await expect(page.getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Ident" })).toHaveCount(0);
  // Attribute columns show the values the list API returns with each CI.
  // (The last cell holds the row's actions.)
  await expect(page.getByRole("row").filter({ hasText: "fra1-esx-01" }).getByRole("cell").nth(-2)).toHaveText("32");
  await expect(page.getByRole("columnheader", { name: /Updated/ })).toHaveAttribute("aria-sort", "descending");
  await expect(page.locator(".pagination select")).toHaveValue("25");
  // Clearing the filter sticks: a reload and Back show the URL as the operator left it.
  await page.getByRole("button", { name: "Remove the lookup value filter" }).click();
  await expect(page).not.toHaveURL(/lookupValueId=/);
  await page.reload();
  await expect(page.getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await expect(page).not.toHaveURL(/lookupValueId=/);
  await page.locator("table.data tbody tr a").first().click();
  await page.goBack();
  await expect(page.getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await expect(page).not.toHaveURL(/lookupValueId=/);
  await snap(page, "customization-list-view");
});

test("list views: an attribute default sort, and attribute column headers sort the class's inventory", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  // Text order would put .100 before .11 before .9; the API sorts IP addresses by address, hostnames case-insensitively.
  for (const [n, host, ip] of [["x", "b-sort", "10.99.0.100"], ["y", "A-sort", "10.99.0.9"], ["z", "c-sort", "10.99.0.11"]]) {
    sortCis.push((await createCi(request, serverId, `e2e-sort-${stamp}-${n}`, { hostname: `${host}-${stamp}`, ip_address: ip })).id);
  }
  await page.goto("/admin/customization/list-views?class=server");
  const customize = page.getByRole("button", { name: "Customize the Server list" });
  if (await customize.isVisible()) await customize.click();
  const sort = page.getByLabel("Default sort");
  await expect(sort.getByRole("option", { name: "Hostname (attribute)" })).toHaveCount(1);
  // The empty choice is the default sort, not a second "Label" next to the built-in field.
  await expect(sort.getByRole("option", { name: "Label", exact: true })).toHaveCount(1);
  await expect(sort.getByRole("option", { name: "Default (label, ascending)" })).toHaveCount(1);
  await sort.selectOption("attributes.hostname");
  await page.getByLabel("Sort direction").selectOption("asc");
  for (const label of ["Hostname (attribute)", "IP address (attribute)"]) {
    await page.getByLabel("Add to Columns").selectOption({ label });
    await page.getByRole("button", { name: "Add", exact: true }).click();
  }
  await save(page, "e2e attribute sort");

  const names = page.locator("table.data tbody tr td:first-child");
  await page.goto(`/cis?classId=${serverId}&q=e2e-sort-${stamp}`);
  await expect(page.getByRole("columnheader", { name: /Hostname/ })).toHaveAttribute("aria-sort", "ascending");
  await expect(names).toHaveText([`e2e-sort-${stamp}-y`, `e2e-sort-${stamp}-x`, `e2e-sort-${stamp}-z`]);

  await page.getByRole("columnheader", { name: /IP address/ }).getByRole("button").click();
  await expect(page).toHaveURL(/sort=attributes\.ip_address/);
  await expect(page.getByRole("columnheader", { name: /IP address/ })).toHaveAttribute("aria-sort", "ascending");
  await expect(names).toHaveText([`e2e-sort-${stamp}-y`, `e2e-sort-${stamp}-z`, `e2e-sort-${stamp}-x`]);
  await page.getByRole("columnheader", { name: /IP address/ }).getByRole("button").click();
  await expect(page).toHaveURL(/sort=-attributes\.ip_address/);
  await expect(names).toHaveText([`e2e-sort-${stamp}-x`, `e2e-sort-${stamp}-z`, `e2e-sort-${stamp}-y`]);
  // The sort survives a reload; another class drops it (it may not have the attribute).
  await page.reload();
  await expect(page.getByRole("columnheader", { name: /IP address/ })).toHaveAttribute("aria-sort", "descending");
  await page.getByLabel("Class", { exact: true }).selectOption({ label: "All classes" });
  await expect(page).not.toHaveURL(/sort=/);
  await expect(page.getByRole("columnheader", { name: /Label/ })).toHaveAttribute("aria-sort", "ascending");
});

test("list views: adding a column to a view without columns keeps the default columns", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  // What migration 0020 writes for an rc.1 default sort, and what an API client or an import may send (GH#168).
  const s = await apiGet<Settings>(request, "/ui-settings");
  const view = { classKey: "server", columns: [], defaultSort: { field: "attributes.ip_address", direction: "asc" } };
  const res = await request.put("/api/v1/ui-settings", {
    data: { version: s.version, settings: { ...s.settings, listViews: [view] }, comment: `e2e view without columns ${stamp}` },
    headers: { "X-CSRF-Token": await csrf(request) },
  });
  expect(res.ok(), `put → ${res.status()} ${await res.text()}`).toBeTruthy();

  await page.goto("/admin/customization/list-views?class=server");
  const list = page.getByRole("list", { name: "Columns" });
  const defaults = ["Label", "Ident", "Class", "Active", "Updated"];
  await expect(page.getByText("No columns chosen: the default columns are shown.")).toBeVisible();
  await expect(list.getByRole("listitem")).toHaveCount(defaults.length);
  await page.getByLabel("Add to Columns").selectOption({ label: "Hostname (attribute)" });
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await expect(list.getByRole("listitem")).toHaveCount(defaults.length + 1);
  await expect(page.getByText("No columns chosen: the default columns are shown.")).toHaveCount(0);
  await save(page, "e2e add column to default view");

  await page.goto(`/cis?classId=${serverId}`);
  await expect(page.locator("table.data thead th")).toHaveText([...defaults, "Hostname"].map((h) => new RegExp(`^${h}`)));
  // The label column still opens the CI.
  await expect(page.locator("table.data tbody tr a").first()).toBeVisible();
});

test("list views: a view without the Label column still shows it first, so every row opens its CI", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  await page.goto("/admin/customization/list-views?class=server");
  const customize = page.getByRole("button", { name: "Customize the Server list" });
  if (await customize.isVisible()) await customize.click();
  await page.getByLabel("Remove Label").click();
  await expect(page.getByText("Label is not chosen: it is shown as the first column anyway")).toBeVisible();
  await expect(page.getByLabel("List preview").getByRole("columnheader").first()).toHaveText("Label");
  await save(page, "e2e list view without label");

  await page.goto(`/cis?classId=${serverId}`);
  // Wait for the view (its Hostname column, page size and sort) and the list it refetches: until the
  // settings load, the inventory shows the default columns and page size.
  await expect(page.getByRole("columnheader", { name: /Hostname/ })).toBeVisible();
  await expect(page.getByRole("columnheader").first()).toHaveText(/^Label/);
  await expect(page.locator("table.data.loading")).toHaveCount(0);
  const rows = page.locator("table.data tbody tr");
  await expect(rows.first()).toBeVisible();
  await expect(rows.filter({ hasNot: page.locator("td:first-child a") })).toHaveCount(0);
  await rows.first().locator("td:first-child a").click();
  await expect(page).toHaveURL(/\/cis\/[0-9a-f-]{36}$/);
});

test("layouts: Edit CI opens the layout editor on a Server, whose layout the detail page and the form follow", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  const ci = (await apiGet<{ data: { id: string; label: string }[] }>(request, `/configuration-items?classId=${serverId}&sort=-updatedAt&limit=1`)).data[0];
  await page.goto("/admin/customization/layouts?class=server");
  await expect(page.getByTestId("layout-status")).toContainText("Server uses the template “Standard” by default.");
  // The editor opens in its own window on the most recently updated Server, on the class's default template.
  const [editor] = await Promise.all([page.waitForEvent("popup"), page.getByRole("button", { name: `Edit CI: ${ci.label}` }).click()]);
  await expect(editor).toHaveURL(new RegExp(`/cis/${ci.id}/layout-editor\\?template=standard$`));
  // Tall enough that a dragged field and the tab it is dropped on are both in view.
  await editor.setViewportSize({ width: 1440, height: 2000 });
  const bar = editor.getByRole("region", { name: "Layout editing" });
  const tabBar = editor.getByRole("group", { name: "Tabs of the layout" });
  const section = (label: string) => editor.getByRole("region", { name: `Section ${label}`, exact: true });
  const field = (label: string) => editor.locator(".le-field").filter({ has: editor.getByRole("button", { name: new RegExp(`^${label}, `) }) });
  await expect(bar).toContainText("Layout editor · Server");

  // A second tab, its section renamed and given a two-column grid.
  await tabBar.getByRole("button", { name: "+ Tab" }).click();
  await tabBar.getByLabel("Tab name").fill("Hardware");
  await editor.keyboard.press("Enter");
  await section("Hardware").getByRole("button", { name: "Hardware", exact: true }).click();
  await editor.getByLabel("Section name").fill("Hardware facts");
  await editor.keyboard.press("Enter");
  await section("Hardware facts").hover();
  await section("Hardware facts").getByLabel("Columns of Hardware facts").selectOption("2");
  await expect(section("Hardware facts")).toContainText("2 columns");

  // CPU cores dragged onto the tab; Manufacturer and Model moved there from their toolbars.
  await tabBar.getByRole("button", { name: "General", exact: true }).click();
  await field("CPU cores").dragTo(tabBar.getByRole("button", { name: "Hardware", exact: true }));
  await expect(section("Hardware facts").locator(".le-field")).toHaveCount(1);
  await tabBar.getByRole("button", { name: "General", exact: true }).click();
  await field("Manufacturer").hover();
  await field("Manufacturer").getByLabel("Move Manufacturer to section").selectOption({ label: "Hardware facts" });
  await tabBar.getByRole("button", { name: "General", exact: true }).click();
  await field("Model").hover();
  await field("Model").getByLabel("Move Model to section").selectOption({ label: "Hardware facts" });
  await expect(section("Hardware facts").locator(".le-field")).toHaveCount(3);
  // Hiding: Delete hides a field; core fields refuse and say why.
  await tabBar.getByRole("button", { name: "General", exact: true }).click();
  await editor.getByRole("button", { name: /^Asset tag, / }).focus();
  await editor.keyboard.press("Delete");
  await expect(editor.getByTestId("le-hidden").getByRole("listitem")).toHaveText([/Asset tag/]);
  await editor.getByRole("button", { name: /^Ident, / }).focus();
  await editor.keyboard.press("Delete");
  await expect(editor.locator(".le-canvas [aria-live=assertive]")).toHaveText("Ident belongs to every CI: it can be moved, not hidden.");
  await expect(field("Ident")).toHaveCount(1);

  await saveLayout(editor, `e2e layout ${stamp}`);
  await snap(editor, "customization-layout-editor");
  await editor.close();
  await page.reload();
  await expect(page.getByTestId("layout-status")).toContainText("Server uses the template “Standard” by default.");

  // The detail page: the layout's tabs, then the relationship map.
  await page.goto(`/cis/${ci.id}`);
  const tabs = page.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(tabs).toHaveText(["General", "Hardware", "Relationship map", "Impact", "History"]);
  await expect(page.locator(".layout-container").getByText("CPU cores", { exact: true })).toHaveCount(0);
  await expect(page.locator(".layout-container").getByText("Asset tag", { exact: true })).toHaveCount(0);
  await tabs.filter({ hasText: "Hardware" }).click();
  await expect(page.locator(".lg-free > details > summary h2")).toHaveText(["Hardware facts"]);
  await expect(fieldLabels(page.locator(".lg-free"))).toHaveText([/^CPU cores/, /^Manufacturer/, /^Model/]);

  // The page is the form: a field on the second tab is edited and saved there, and the page stays as it is.
  await expect(page.getByLabel("Asset tag")).toHaveCount(0);
  await page.getByLabel("Model").fill(`E2E ${stamp}`);
  await saveCi(page);
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  await expect(page.getByLabel("Model")).toHaveValue(`E2E ${stamp}`);
  expect(await ciIdByName(request, ci.label)).toBe(ci.id);
  await snap(page, "customization-layout");
});

test("history: an earlier version can be restored", async ({ page }) => {
  await page.goto("/admin/customization/history");
  const rows = page.locator("tbody tr");
  await expect(rows.first()).toContainText("current");
  await expect(rows.first()).toContainText(`e2e layout ${stamp}`);
  // Restore the version saved before the layout (the list view).
  await rows.filter({ hasText: `e2e list view ${stamp}` }).getByRole("button", { name: "Restore" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Restore" }).click();
  await expect(rows.first()).toContainText("Restored version");
  await expect(rows.first()).toContainText("current");
  await page.goto("/admin/customization/layouts?class=server");
  await expect(page.getByTestId("layout-status")).toContainText("Server uses the template “Standard” by default.");
});

test("layouts: the API validates the layout document and converts the older panels format", async ({ request }) => {
  const s = await apiGet<Settings>(request, "/ui-settings");
  const headers = { "X-CSRF-Token": await csrf(request) };
  const put = (layout: object) => request.put("/api/v1/ui-settings", { data: { version: s.version, settings: { ...s.settings, layouts: [layout] } }, headers });
  const section = (fields: object[], columns = 2) => ({ key: "s", label: "S", columns, fields });
  const bad = await put({
    classKey: "server",
    tabs: [
      { key: "a", label: "A", sections: [section([{ field: "attributes.model", width: 3 }])] },
      { key: "a", label: "B", sections: [{ ...section([{ field: "attributes.model" }]), key: "t" }] },
    ],
    hiddenFields: ["validFrom"],
  });
  expect(bad.status()).toBe(400);
  const fields = ((await bad.json()).error.details as { field: string }[]).map((d) => d.field);
  expect(fields).toEqual([
    "settings.layouts.0.tabs.0.sections.0.fields.0.width",
    "settings.layouts.0.tabs.1.key",
    "settings.layouts.0.tabs.1.sections.0.fields.0.field",
    "settings.layouts.0.hiddenFields.0",
  ]);
  // Unchanged settings: nothing was saved.
  expect((await apiGet<Settings>(request, "/ui-settings")).version).toBe(s.version);

  // The layout format before tabs (older exports and API clients) is still accepted, and converted.
  const v1 = await put({ classKey: "server", panels: [{ key: "main", label: "Main", fields: ["attributes.model"] }] });
  expect(v1.ok(), await v1.text()).toBeTruthy();
  const saved = (await v1.json()).settings.layouts[0];
  expect(saved.panels).toBeUndefined();
  // Every tab is stored free (SHAA-1471): the section is a window at the full width.
  expect(saved.tabs).toEqual([
    {
      key: "general",
      label: "General",
      placement: "free",
      sections: [{ key: "main", label: "Main", columns: 3, width: 12, collapsed: false, fields: [{ field: "attributes.model", width: 1 }], frame: { x: 0, y: 0, w: 1, h: 96, z: 1 } }],
    },
  ]);
});

test("a concurrent save is reported, not overwritten", async ({ page, request }) => {
  await page.goto("/admin/customization/branding");
  await page.getByLabel("Application name").fill(`${APP} mine`);
  // Someone else saves in between.
  const s = await apiGet<Settings>(request, "/ui-settings");
  const res = await request.put("/api/v1/ui-settings", {
    data: { version: s.version, settings: { ...s.settings, branding: { ...(s.settings.branding as object), appName: `${APP} theirs` } } },
    headers: { "X-CSRF-Token": await csrf(request) },
  });
  expect(res.ok()).toBeTruthy();
  await page.getByRole("region", { name: "Save changes" }).getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "Someone else saved the settings" })).toBeVisible();
  await page.getByRole("button", { name: "Load the latest version" }).click();
  await expect(page.getByLabel("Application name")).toHaveValue(`${APP} theirs`);
  await expect(page.getByRole("region", { name: "Save changes" }).getByText("No unsaved changes")).toBeVisible();
});

test("export/import: download, dry run shows the diff, apply changes the app", async ({ page }) => {
  await page.goto("/admin");
  await page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Export / import" }).click();
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Download configuration" }).click()]);
  const file = JSON.parse(await (await download.createReadStream()).toArray().then((c) => Buffer.concat(c).toString("utf8")));
  expect(file.format).toBe("shadoucmdb.config");
  expect(file.uiSettings.settings.branding.appName).toBe(`${APP} theirs`);
  expect(JSON.stringify(file)).not.toContain("admin-password");

  const input = page.locator("#config-file");
  // The own export changes nothing.
  await input.setInputFiles({ name: "same.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(file)) });
  await expect(page.getByText("Importing this file changes nothing")).toBeVisible();
  await expect(page.getByRole("button", { name: "Apply import" })).toBeDisabled();

  // Not JSON, and a file the API rejects: explained, nothing changes.
  await input.setInputFiles({ name: "junk.json", mimeType: "application/json", buffer: Buffer.from("{nope") });
  await expect(page.getByText("junk.json is not a JSON file")).toBeVisible();
  const broken = structuredClone(file);
  broken.dataModel.attributes.push({ ...broken.dataModel.attributes[0], class: `missing_${stamp}` });
  await input.setInputFiles({ name: "broken.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(broken)) });
  await expect(page.getByText("The file cannot be imported")).toBeVisible();
  await expect(page.getByRole("alert").locator("code").first()).toContainText("dataModel.attributes");

  // A real change: another app name and a new lookup list.
  const changed = structuredClone(file);
  changed.uiSettings.settings.branding.appName = `${APP} imported`;
  changed.lookups.lists.push({ key: `e2e_${stamp}`, name: `E2E list ${stamp}`, isActive: true, sortOrder: 999, values: [] });
  await input.setInputFiles({ name: "changed.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(changed)) });
  await expect(page.getByRole("table", { name: "Import summary" })).toBeVisible();
  await expect(page.locator(".import-changes").filter({ hasText: "Lookup lists" })).toContainText(`e2e_${stamp}`);
  await expect(page.locator(".import-changes").filter({ hasText: "UI settings" })).toContainText(`${APP} imported`);
  await snap(page, "config-import-dry-run");
  await page.getByRole("button", { name: "Apply import" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Apply import" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Imported changed.json" })).toBeVisible();
  await expect(page.locator(".shell-brand")).toContainText(`${APP} imported`);
});

test("export/import: an older file's former lookup sections are listed as warnings, in the dry run and after applying", async ({ page }) => {
  // What the API answers for a 0.1.0-rc.1 file: its statuses … owners become the lookup lists of the same name.
  const warnings = ["statuses", "environments", "locations", "owners"].map((s) => ({
    path: `lookups.${s}`,
    message: `Imported as the lookup list “${s}” (the former table is not written).`,
  }));
  const result = (mode: string) => ({
    mode,
    applied: mode === "apply",
    schemaChanges: [],
    summary: [{ section: "lookupLists", created: 1, updated: 0, deleted: 0, unchanged: 0, notInFile: 0 }],
    changes: [{ section: "lookupLists", key: "status", action: "create", fields: [] }],
    warnings,
    uiSettingsIssues: [],
  });
  await page.route("**/api/v1/admin/config/import?*", (route) =>
    route.fulfill({ json: result(new URL(route.request().url()).searchParams.get("mode") ?? "dry_run") }),
  );
  await page.goto("/admin/config");
  await expect(page.getByText("the lookup lists and their values (such as status, environment, location and owner)")).toBeVisible();
  await page.locator("#config-file").setInputFiles({ name: "rc1.json", mimeType: "application/json", buffer: Buffer.from("{}") });
  const dry = page.getByRole("status").filter({ hasText: "Worth a look before applying" });
  for (const w of warnings) await expect(dry.locator("li").filter({ hasText: w.message })).toContainText(w.path);
  await expect(page.locator(".import-changes").filter({ hasText: "Lookup lists" })).toContainText("status");
  await page.getByRole("button", { name: "Apply import" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Apply import" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Imported rc1.json" })).toBeVisible();
  const after = page.getByRole("status").filter({ hasText: "Imported with warnings" });
  for (const w of warnings) await expect(after.locator("li").filter({ hasText: w.message })).toContainText(w.path);
});
