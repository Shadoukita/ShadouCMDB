import type { APIRequestContext, Page } from "@playwright/test";
import { apiGet, ciIdByName, classIdByName, expect, snap, test } from "./support";

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
  assets: { logo: unknown; favicon: unknown };
}

async function csrf(request: APIRequestContext) {
  return (await request.storageState()).cookies.find((c) => c.name === "shadoucmdb_csrf")?.value ?? "";
}

/** Back to the built-in settings and no images. */
async function resetSettings(request: APIRequestContext) {
  const s = await apiGet<Settings>(request, "/ui-settings");
  const headers = { "X-CSRF-Token": await csrf(request) };
  if (JSON.stringify(s.settings) !== JSON.stringify(EMPTY)) {
    const res = await request.put("/api/v1/ui-settings", { data: { version: s.version, settings: {}, comment: "e2e reset" }, headers });
    expect(res.ok(), `reset → ${res.status()} ${await res.text()}`).toBeTruthy();
  }
  for (const kind of ["logo", "favicon"] as const) {
    if (s.assets[kind]) expect((await request.delete(`/api/v1/ui-settings/assets/${kind}`, { headers })).ok()).toBeTruthy();
  }
}
const EMPTY = {
  branding: { appName: null, primaryColor: null, accentColor: null, defaultTheme: "system" },
  navigation: { entries: [] },
  dashboard: { widgets: null },
  listViews: [],
  layouts: [],
};

async function save(page: Page, comment: string) {
  const bar = page.getByRole("region", { name: "Save changes" });
  await expect(bar.getByText("Unsaved changes")).toBeVisible();
  await bar.getByLabel("Comment for this version").fill(`${comment} ${stamp}`);
  await bar.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status").filter({ hasText: /Saved as version \d+/ })).toBeVisible();
  await expect(bar.getByText("No unsaved changes")).toBeVisible();
}

test.beforeAll(async ({ request }) => resetSettings(request));
test.afterAll(async ({ request }) => resetSettings(request));

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
  await page.getByLabel("Theme").selectOption("light");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.getByLabel("Theme").selectOption("");
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
  await row.getByRole("group", { name: "Statuses" }).getByLabel("In service").check();
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
  await page.getByLabel("Remove Owner").click();
  await page.getByLabel("Add to Columns").selectOption({ label: "CPU cores (attribute)" });
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await page.getByLabel("Default sort").selectOption("updatedAt");
  await page.getByLabel("Sort direction").selectOption("desc");
  await page.getByLabel("Rows per page (10-200)").fill("25");
  await page.getByLabel("Rows per page (10-200)").blur();
  await page.getByRole("group", { name: "Default statuses" }).getByLabel("In service").check();
  await expect(page.getByLabel("List preview").getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await save(page, "e2e list view");

  // Opened from the menu, the list gets the default filter in its URL: it shows in the toolbar and can be changed.
  await page.goto("/");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: /^Server/ }).click();
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page).toHaveURL(/statusId=/);
  await expect(page.locator("#f-status")).not.toHaveValue("");
  await expect(page.getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Owner" })).toHaveCount(0);
  await expect(page.getByRole("columnheader", { name: /Updated/ })).toHaveAttribute("aria-sort", "descending");
  await expect(page.locator(".pagination select")).toHaveValue("25");
  // Clearing the filter sticks: a reload and Back show the URL as the operator left it.
  await page.locator("#f-status").selectOption("");
  await expect(page).not.toHaveURL(/statusId=/);
  await page.reload();
  await expect(page.getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await expect(page).not.toHaveURL(/statusId=/);
  await page.locator("table.data tbody tr a").first().click();
  await page.goBack();
  await expect(page.getByRole("columnheader", { name: "CPU cores" })).toBeVisible();
  await expect(page).not.toHaveURL(/statusId=/);
  await snap(page, "customization-list-view");
});

test("layouts: panels, hidden and read-only fields on the detail page and the form", async ({ page, request }) => {
  await page.goto("/admin/customization/layouts?class=server");
  await page.getByRole("button", { name: "Customize the Server layout" }).click();
  await page.getByLabel("New panel").fill("Hardware facts");
  await page.getByRole("button", { name: "Add panel" }).click();
  const add = page.getByLabel("Add to Fields of Hardware facts");
  for (const label of ["Manufacturer (attribute)", "Model (attribute)", "CPU cores (attribute)"]) {
    await add.selectOption({ label });
    await page.locator(".layout-editor-panel").getByRole("button", { name: "Add", exact: true }).click();
  }
  await page.getByLabel("Hide Asset tag").check();
  await page.getByLabel("Make Serial number read-only").check();
  await expect(page.getByLabel("Layout preview").getByRole("heading").first()).toHaveText(/Hardware facts/);
  await save(page, "e2e layout");

  const servers = await apiGet<{ data: { id: string; name: string }[] }>(request, `/configuration-items?classId=${await classIdByName(request, "Server")}&limit=1`);
  const ci = servers.data[0];
  await page.goto(`/cis/${ci.id}`);
  const panels = page.locator(".layout-panels > details > summary h2");
  await expect(panels.first()).toHaveText(/Hardware facts/);
  await expect(page.locator(".layout-panels").getByText("Asset tag", { exact: true })).toHaveCount(0);
  await expect(page.locator(".layout-panels").getByText("CPU cores", { exact: true })).toBeVisible();

  await page.getByRole("link", { name: "Edit" }).click();
  await expect(page.getByLabel("Serial number")).toBeDisabled();
  await expect(page.getByLabel("Asset tag")).toHaveCount(0);
  await page.getByLabel("Model").fill(`E2E ${stamp}`);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  await expect(page.locator(".layout-panels")).toContainText(`E2E ${stamp}`);
  expect(await ciIdByName(request, ci.name)).toBe(ci.id);
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
  await expect(page.getByRole("button", { name: "Customize the Server layout" })).toBeVisible();
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

  // A real change: another app name and a new status.
  const changed = structuredClone(file);
  changed.uiSettings.settings.branding.appName = `${APP} imported`;
  changed.lookups.statuses.push({ key: `e2e_${stamp}`, name: `E2E status ${stamp}`, isOperational: false, isActive: true, sortOrder: 999 });
  await input.setInputFiles({ name: "changed.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(changed)) });
  await expect(page.getByRole("table", { name: "Import summary" })).toBeVisible();
  await expect(page.locator(".import-changes").filter({ hasText: "Statuses" })).toContainText(`e2e_${stamp}`);
  await expect(page.locator(".import-changes").filter({ hasText: "UI settings" })).toContainText(`${APP} imported`);
  await snap(page, "config-import-dry-run");
  await page.getByRole("button", { name: "Apply import" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Apply import" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Imported changed.json" })).toBeVisible();
  await expect(page.locator(".shell-brand")).toContainText(`${APP} imported`);
});
