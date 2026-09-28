import type { APIRequestContext, Page } from "@playwright/test";
import { apiGet, ciIdByName, classIdByName, createCi, csrf, expect, resetUiSettings as resetSettings, snap, test } from "./support";

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
  await expect(page.getByRole("row").filter({ hasText: "fra1-esx-01" }).getByRole("cell").last()).toHaveText("32");
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
    await createCi(request, serverId, `e2e-sort-${stamp}-${n}`, { hostname: `${host}-${stamp}`, ip_address: ip });
  }
  await page.goto("/admin/customization/list-views?class=server");
  const customize = page.getByRole("button", { name: "Customize the Server list" });
  if (await customize.isVisible()) await customize.click();
  const sort = page.getByLabel("Default sort");
  await expect(sort.getByRole("option", { name: "Hostname (attribute)" })).toHaveCount(1);
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

test("layouts: the form designer arranges tabs, sections and widths for the form and the detail page", async ({ page, request }) => {
  // Tall enough that a dragged field and the tab it is dropped on are both in view.
  await page.setViewportSize({ width: 1440, height: 1800 });
  await page.goto("/admin/customization/layouts?class=server");
  await page.getByRole("button", { name: "Customize the Server layout" }).click();
  const frame = page.getByTestId("designer-frame");
  const chip = (label: string) => frame.locator(".designer-field").filter({ has: page.locator(".designer-label", { hasText: new RegExp(`^${label}\\*?$`) }) });
  const props = page.getByRole("complementary", { name: "Layout properties" });
  const say = page.locator(".designer [aria-live=assertive]");
  // The live preview is the Server form: one General tab, the core fields first.
  await expect(frame.getByRole("tab")).toHaveText(["General"]);
  await expect(frame.locator(".designer-section").first().locator(".designer-label").first()).toHaveText("Ident");
  await expect(chip("Ident").locator("input")).toHaveAttribute("placeholder", "Generated");

  // A second tab, renamed, whose section gets a two-column grid.
  await frame.getByRole("button", { name: "+ Add tab" }).click();
  await props.getByLabel("Tab name").fill("Hardware");
  await expect(frame.getByRole("tab")).toHaveText(["General", "Hardware"]);
  await frame.getByRole("button", { name: "Tab 2", exact: true }).click();
  await props.getByLabel("Section heading").fill("Hardware facts");
  await props.getByLabel("Columns").selectOption("2");
  await expect(frame.locator(".designer-section").first()).toContainText("2 columns");

  // Drag and drop: CPU cores onto the Hardware tab, then Manufacturer before it in the section.
  await frame.getByRole("tab", { name: "General" }).click();
  await chip("CPU cores").dragTo(frame.getByRole("tab", { name: "Hardware" }));
  await expect(frame.getByRole("tab", { name: "Hardware" })).toHaveAttribute("aria-selected", "true");
  await expect(frame.locator(".designer-section").first().locator(".designer-label")).toHaveText(["CPU cores"]);
  await frame.getByRole("tab", { name: "General" }).click();
  await chip("Manufacturer").dragTo(frame.getByRole("tab", { name: "Hardware" }));
  await chip("Manufacturer").dragTo(chip("CPU cores"), { targetPosition: { x: 5, y: 20 } });
  await expect(frame.locator(".designer-section").first().locator(".designer-label")).toHaveText(["Manufacturer", "CPU cores"]);

  // The keyboard alternative: select with Enter, move with the side panel and Alt+arrows, resize with Alt+arrows.
  await frame.getByRole("tab", { name: "General" }).click();
  await chip("Model").focus();
  await page.keyboard.press("Enter");
  await props.getByLabel("Section").selectOption({ label: "Hardware facts" });
  await expect(frame.getByRole("tab", { name: "Hardware" })).toHaveAttribute("aria-selected", "true");
  await expect(frame.locator(".designer-section").first().locator(".designer-label")).toHaveText(["Manufacturer", "CPU cores", "Model"]);
  await chip("Model").focus();
  await page.keyboard.press("Alt+ArrowUp");
  await expect(say).toHaveText("Model moved to position 2 of 3 in Hardware facts.");
  await expect(frame.locator(".designer-section").first().locator(".designer-label")).toHaveText(["Manufacturer", "Model", "CPU cores"]);
  await expect(chip("Model")).toBeFocused();
  await page.keyboard.press("Alt+ArrowRight");
  await expect(chip("Model")).toHaveAttribute("aria-label", /Model, 2 of 2 columns/);
  await page.keyboard.press("Alt+ArrowRight");
  await expect(say).toHaveText("Model: 2 of 2 columns.");
  // Resize with the mouse: drag CPU cores' right edge across the second column.
  const cpu = chip("CPU cores");
  const box = (await cpu.boundingBox())!;
  const handle = (await cpu.locator(".resize-handle").boundingBox())!;
  await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2);
  await page.mouse.down();
  await page.mouse.move(handle.x + box.width, handle.y + handle.height / 2, { steps: 5 });
  await page.mouse.up();
  await expect(cpu).toHaveAttribute("aria-label", /CPU cores, 2 of 2 columns/);
  await expect(cpu).toHaveClass(/lg-w-2/);

  // Hiding: Delete hides a field; core fields refuse and say why.
  await frame.getByRole("tab", { name: "General" }).click();
  await chip("Asset tag").focus();
  await page.keyboard.press("Delete");
  await expect(page.getByTestId("designer-hidden").getByRole("listitem")).toHaveText([/Asset tag/]);
  await chip("Ident").focus();
  await page.keyboard.press("Delete");
  await expect(say).toHaveText("Ident belongs to every CI: it can be moved, not hidden.");
  await expect(chip("Ident")).toHaveCount(1);
  await chip("Ident").click();
  await expect(props.getByRole("button", { name: "Hide field" })).toBeDisabled();
  await chip("Serial number").click();
  await props.getByLabel("Read-only on the form").check();
  await expect(chip("Serial number")).toContainText("read-only");

  // The preview can be narrowed to check small screens: the grid falls back to one column.
  await page.getByRole("toolbar", { name: "Preview width" }).getByRole("button", { name: "Phone 390" }).click();
  await expect(page.getByTestId("designer-width")).toHaveText(/^3\d\d px wide$/);
  const narrow = (await chip("Ident").boundingBox())!;
  const next = (await chip("Valid from").boundingBox())!;
  expect(next.y).toBeGreaterThan(narrow.y + narrow.height - 1);
  await snap(page, "customization-layout-designer-phone");
  await page.getByRole("toolbar", { name: "Preview width" }).getByRole("button", { name: "Full width" }).click();
  await frame.getByRole("tab", { name: "Hardware" }).click();
  await snap(page, "customization-layout-designer");

  // Unsaved changes are guarded when leaving Customization (its sections share one draft).
  let asked = "";
  page.once("dialog", (d) => {
    asked = d.message();
    void d.dismiss();
  });
  await page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Users" }).click();
  await expect(page).toHaveURL(/\/admin\/customization\/layouts/);
  expect(asked).toContain("unsaved");
  await save(page, "e2e layout");

  // The detail page: the layout's tabs, then the relationship map.
  const servers = await apiGet<{ data: { id: string; label: string }[] }>(request, `/configuration-items?classId=${await classIdByName(request, "Server")}&limit=1`);
  const ci = servers.data[0];
  await page.goto(`/cis/${ci.id}`);
  const tabs = page.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(tabs).toHaveText(["General", "Hardware", "Relationship map", "History"]);
  await expect(page.locator(".layout-panels").getByText("CPU cores", { exact: true })).toHaveCount(0);
  await expect(page.locator(".layout-panels").getByText("Asset tag", { exact: true })).toHaveCount(0);
  await tabs.filter({ hasText: "Hardware" }).click();
  await expect(page.locator(".layout-panels > details > summary h2")).toHaveText(["Hardware facts"]);
  await expect(page.locator(".layout-panels dt")).toHaveText(["Manufacturer", "Model", "CPU cores"]);

  // The form: the same tabs; a field on the second tab is edited and saved.
  await page.getByRole("link", { name: "Edit" }).click();
  const formTabs = page.getByRole("tablist", { name: "Form tabs" }).getByRole("tab");
  await expect(formTabs).toHaveText(["General", "Hardware"]);
  await expect(page.getByLabel("Serial number")).toBeDisabled();
  await expect(page.getByLabel("Asset tag")).toHaveCount(0);
  await expect(page.getByLabel("Model")).toBeHidden();
  await formTabs.filter({ hasText: "Hardware" }).click();
  await page.getByLabel("Model").fill(`E2E ${stamp}`);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  await tabs.filter({ hasText: "Hardware" }).click();
  await expect(page.locator(".layout-panels")).toContainText(`E2E ${stamp}`);
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
  await expect(page.getByRole("button", { name: "Customize the Server layout" })).toBeVisible();
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
  expect(saved.tabs).toEqual([{ key: "general", label: "General", sections: [{ key: "main", label: "Main", columns: 3, width: 12, collapsed: false, fields: [{ field: "attributes.model", width: 1 }] }] }]);
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
