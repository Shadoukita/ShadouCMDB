import type { Browser, Locator, Page } from "@playwright/test";
import { apiGet, apiSend, classIdByName, csrf, expect, fieldLabels, resetUiSettings, saveLayout, snap, test } from "./support";

// Edit layout: the Server layout edited on a real Server CI (the demo seed) in the layout editor's
// own window, saved as a settings version, and seen by a user who may only view servers. The walk starts from and ends
// with the built-in settings, so the other specs see the stock UI.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const VIEWER = `e2e-layout-viewer-${stamp}`;
const PASSWORD = "layout-viewer-password-123";
let ci: { id: string; label: string; ident: string };

test.beforeAll(async ({ request }) => {
  await resetUiSettings(request);
  const serverId = await classIdByName(request, "Server");
  ci = (await apiGet<{ data: { id: string; label: string; ident: string }[] }>(request, `/configuration-items?classId=${serverId}&sort=label&limit=1`)).data[0];
  // Views servers, nothing else: no customization.manage.
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E layout viewers ${stamp}`,
    globalPermissions: [],
    classPermissions: [{ classId: serverId, view: true, create: false, edit: false, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: VIEWER, email: `${VIEWER}@example.test`, displayName: `E2E Layout viewer ${stamp}`, password: PASSWORD, profileIds: [profile.id] });
});
test.afterAll(async ({ request }) => resetUiSettings(request));

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

/** Clicks what opens the layout editor (the page's Edit layout by default), and returns its window once the editor shows. */
async function openEditor(page: Page, opener: Locator = page.getByRole("button", { name: "Edit layout" })): Promise<Page> {
  const [popup] = await Promise.all([page.waitForEvent("popup"), opener.click()]);
  await expect(bar(popup)).toBeVisible();
  return popup;
}

/** Done in the editor's window closes it. */
async function closeEditor(page: Page) {
  const closed = page.waitForEvent("close");
  // The window closes during the click, which Playwright reports as an error of the click itself.
  await bar(page).getByRole("button", { name: "Done" }).click().catch(() => undefined);
  await closed;
}

/** A section of the detail page by its heading. */
const pagePanel = (page: Page, heading: string) => page.locator(".layout-container .layout-panel").filter({ has: page.locator(".panel-header h2", { hasText: new RegExp(`^${heading}$`) }) });
const bar = (page: Page) => page.getByRole("region", { name: "Layout editing" });
const tabBar = (page: Page) => page.getByRole("group", { name: "Tabs of the layout" });
const section = (page: Page, label: string) => page.getByRole("region", { name: `Section ${label}`, exact: true });
/** A field on the canvas by its label (its grip's accessible name starts with it). */
const field = (page: Page, label: string) => page.locator(".le-field").filter({ has: page.getByRole("button", { name: new RegExp(`^${label}, `) }) });

test("the editor window: add a tab and a section, move fields, save, and a viewer sees the result", async ({ page: origin, browser }) => {
  await origin.goto(`/cis/${ci.id}`);
  const page = await openEditor(origin);
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}/layout-editor$`));
  // The page it was opened from stays as it is; a second click focuses the open editor instead of another one.
  await expect(origin.getByRole("tablist", { name: "CI sections" })).toBeVisible();
  await expect(bar(origin)).toHaveCount(0);
  await origin.getByRole("button", { name: "Edit layout" }).click();
  await expect.poll(() => origin.context().pages().length).toBe(2);
  // Tall enough that a dragged field and where it is dropped are both in view.
  await page.setViewportSize({ width: 1440, height: 3200 });
  // The mode is unmistakable, and says what the change applies to.
  await expect(bar(page)).toContainText("Layout editor · Server");
  await expect(bar(page).getByTestId("le-target")).toContainText("Template: Standard");
  await expect(bar(page)).toContainText("Saving to the template changes every class and CI that uses it");
  await expect(bar(page).getByText("Unsaved changes")).toHaveCount(0);
  // The canvas is the real page: the CI's own values, the built-in General tab, its sections as windows.
  await expect(tabBar(page).getByRole("button", { name: "General", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(field(page, "Ident")).toContainText(ci.ident);
  await expect(page.getByRole("tablist", { name: "CI sections" })).toHaveCount(0);
  await expect(page.locator('[data-window="general"]')).toBeVisible();
  // One placement only: no Grid / Free switch, no preview widths, no designer; snapping and layers always there.
  for (const name of ["Grid", "Free", "Desktop", "Phone", "Open in the designer"]) await expect(bar(page).getByRole("button", { name, exact: true })).toHaveCount(0);
  await expect(bar(page).getByRole("link", { name: "Open in the designer" })).toHaveCount(0);
  await expect(bar(page).getByRole("button", { name: "Snap", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(bar(page).getByTestId("le-layer")).toHaveText("No window selected");

  // + Tab: added at the end of the tab bar, named in place.
  await tabBar(page).getByRole("button", { name: "Add a tab" }).click();
  await expect(tabBar(page).getByLabel("Tab name")).toBeFocused();
  await tabBar(page).getByLabel("Tab name").fill("Hardware");
  await page.keyboard.press("Enter");
  await expect(tabBar(page).getByRole("button", { name: "Hardware", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(bar(page).getByText("Unsaved changes")).toBeVisible();
  // Its section is renamed by clicking its heading.
  await section(page, "Hardware").getByRole("button", { name: "Hardware", exact: true }).click();
  await page.getByLabel("Section name").fill("Hardware facts");
  await page.keyboard.press("Enter");
  await expect(section(page, "Hardware facts")).toBeVisible();

  // + Section on the General tab, named in place.
  await tabBar(page).getByRole("button", { name: "General", exact: true }).click();
  await page.getByRole("button", { name: "Add a section to General" }).click();
  await page.getByLabel("Section name").fill("Lifecycle");
  await page.keyboard.press("Enter");
  await expect(section(page, "Lifecycle")).toBeVisible();

  // Drag and drop: CPU cores onto the Hardware tab, Warranty end into Lifecycle.
  await field(page, "CPU cores").dragTo(tabBar(page).getByRole("button", { name: "Hardware", exact: true }));
  await expect(tabBar(page).getByRole("button", { name: "Hardware", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(section(page, "Hardware facts").locator(".le-field")).toHaveCount(1);
  await expect(field(page, "CPU cores")).toBeVisible();
  await tabBar(page).getByRole("button", { name: "General", exact: true }).click();
  await field(page, "Warranty end").dragTo(section(page, "Lifecycle").getByText("Drop fields here"));
  await expect(section(page, "Lifecycle").locator(".le-field")).toHaveCount(1);

  // The keyboard alternative: the field's toolbar moves Model to another section, Alt+arrows resize it.
  await field(page, "Model").getByLabel("Move Model to section").selectOption({ label: "Hardware facts" });
  await expect(section(page, "Hardware facts").locator(".le-field")).toHaveCount(2);
  await expect(page.getByRole("button", { name: /^Model, / })).toBeFocused();
  await page.keyboard.press("Alt+ArrowUp");
  await expect(page.locator(".le-canvas [aria-live=assertive]")).toHaveText("Model moved to position 1 of 2 in Hardware facts.");
  await page.keyboard.press("Alt+ArrowRight");
  await expect(page.getByRole("button", { name: /^Model, / })).toHaveAccessibleName(/Model, 2 of 3 columns/);
  // Hide with Delete; the hidden-fields tray lists it and can show it again.
  await tabBar(page).getByRole("button", { name: "General", exact: true }).click();
  await page.getByRole("button", { name: /^Asset tag, / }).focus();
  await page.keyboard.press("Delete");
  await expect(page.getByTestId("le-hidden").getByRole("listitem")).toHaveText([/Asset tag/]);
  // GH#192: hiding is presentation only, and the tray says so.
  await expect(page.getByTestId("le-presentation-only")).toContainText("use permission profiles");

  // Undo and redo.
  await bar(page).getByRole("button", { name: "Undo" }).click();
  await expect(page.getByTestId("le-hidden").getByRole("listitem")).toHaveCount(0);
  await bar(page).getByRole("button", { name: "Redo" }).click();
  await expect(page.getByTestId("le-hidden").getByRole("listitem")).toHaveText([/Asset tag/]);

  await snap(page, "layout-edit-detail");

  // Unsaved changes are guarded: closing the editor asks first.
  let asked = "";
  page.once("dialog", (d) => {
    asked = d.message();
    void d.dismiss();
  });
  await bar(page).getByRole("button", { name: "Done" }).click();
  expect(asked).toContain("unsaved");
  await expect(bar(page)).toBeVisible();

  // Save with a note: a new settings version.
  await saveLayout(page, `e2e in-place layout ${stamp}`);
  await expect(bar(page).getByText("Unsaved changes")).toHaveCount(0);
  // The page the editor was opened from shows the saved layout without a reload.
  const tabs = origin.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(tabs).toHaveText(["General", "Hardware", "Relationship map", "Impact", "History"]);
  // Done closes the editor's window.
  await closeEditor(page);

  // The version history has the note.
  const versions = await apiGet<{ data: { comment: string | null }[] }>(origin.request, "/ui-settings/versions?limit=1");
  expect(versions.data[0].comment).toBe(`e2e in-place layout ${stamp}`);

  // A user who may only view servers sees the new layout, and no way to edit it.
  const viewer = await signInUi(browser, VIEWER, PASSWORD);
  await viewer.goto(`/cis/${ci.id}`);
  const vtabs = viewer.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(vtabs).toHaveText(["General", "Hardware", "Relationship map", "Impact"]);
  await expect(viewer.locator(".lg-free > .layout-panel > .panel-header h2")).toContainText(["Lifecycle"]);
  await expect(viewer.locator(".layout-container").getByText("Asset tag", { exact: true })).toHaveCount(0);
  await expect(viewer.getByRole("button", { name: "Edit layout" })).toHaveCount(0);
  await vtabs.filter({ hasText: "Hardware" }).click();
  await expect(viewer.locator(".lg-free > .layout-panel > .panel-header h2")).toHaveText(["Hardware facts"]);
  await expect(fieldLabels(viewer.locator(".lg-free"))).toHaveText(["Model", "CPU cores"]);
  // Read-only for them, in the same place.
  await expect(viewer.locator(".lg-free .field-ro")).toHaveCount(2);
  // The editor's URL is just the page for them.
  await viewer.goto(`/cis/${ci.id}/layout-editor`);
  await expect(viewer).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  await expect(viewer.getByRole("tablist", { name: "CI sections" })).toBeVisible();
  await expect(viewer.getByRole("region", { name: "Layout editing" })).toHaveCount(0);
  await snap(viewer, "layout-edit-viewer");
  await viewer.context().close();
});

test("the editor on the form: the CI's values in place, read-only fields, and back to the built-in layout", async ({ page: origin }) => {
  await origin.goto(`/cis/${ci.id}/edit`);
  let page = await openEditor(origin);
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}/edit/layout-editor$`));
  await expect(bar(page)).toContainText("Layout editor · Server");
  // The form's own controls, inert, with the CI's values.
  await expect(field(page, "Ident").locator("#f-ident")).toHaveValue(ci.ident);
  await expect(page.getByRole("button", { name: "Save changes" })).toHaveCount(0);
  // The field's toolbar shows while it is hovered (or holds the keyboard focus).
  await field(page, "Serial number").hover();
  await field(page, "Serial number").getByLabel("Read-only").check();
  await expect(field(page, "Serial number")).toContainText("read-only");
  await saveLayout(page);
  await closeEditor(page);
  await expect(origin.getByLabel("Serial number")).toBeDisabled();
  await expect(origin.getByRole("tablist", { name: "Form tabs" }).getByRole("tab")).toHaveText(["General", "Hardware"]);

  // Reset to the built-in layout (undoable until saved).
  page = await openEditor(origin);
  await bar(page).getByRole("button", { name: "Reset to built-in layout" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Reset to built-in layout" }).click();
  await expect(bar(page)).toContainText("built-in layout");
  await expect(tabBar(page).getByRole("button", { name: "Hardware", exact: true })).toHaveCount(0);
  await bar(page).getByRole("button", { name: "Undo" }).click();
  await expect(tabBar(page).getByRole("button", { name: "Hardware", exact: true })).toBeVisible();
  await expect(bar(page).getByText("Unsaved changes")).toHaveCount(0);
  // Redo resets again; Discard goes back to the saved layout.
  await bar(page).getByRole("button", { name: "Redo" }).click();
  await expect(bar(page).getByText("Unsaved changes")).toBeVisible();
  await bar(page).getByRole("button", { name: "Discard" }).click();
  await expect(tabBar(page).getByRole("button", { name: "Hardware", exact: true })).toBeVisible();
  await expect(bar(page).getByText("Unsaved changes")).toHaveCount(0);
  await snap(page, "layout-edit-form");
});

test("a concurrent save is reported with a way to reload", async ({ page, request }) => {
  await page.goto(`/cis/${ci.id}/layout-editor`);
  await expect(bar(page)).toBeVisible();
  await field(page, "Manufacturer").hover();
  await field(page, "Manufacturer").getByRole("button", { name: "Make Manufacturer wider" }).click();
  // Someone else saves in between.
  const s = await apiGet<{ version: number; settings: Record<string, unknown> }>(request, "/ui-settings");
  const res = await request.put("/api/v1/ui-settings", {
    data: { version: s.version, settings: { ...s.settings, branding: { ...(s.settings.branding as object), appName: `E2E ${stamp}` } } },
    headers: { "X-CSRF-Token": await csrf(request) },
  });
  expect(res.ok()).toBeTruthy();
  await bar(page).getByTestId("le-save").click();
  await page.getByRole("dialog", { name: /^Save to the template/ }).getByRole("button", { name: "Save to template", exact: true }).click();
  await expect(bar(page).getByRole("alert").filter({ hasText: "Someone else saved while you were editing" })).toBeVisible();
  await bar(page).getByRole("button", { name: "Load the latest version" }).click();
  await expect(bar(page).getByText("Unsaved changes")).toHaveCount(0);
  await expect(bar(page).getByRole("alert")).toHaveCount(0);
});

test("Customization › Layouts: Edit CI opens the editor on the most recently updated CI, another one, or the create form", async ({ page: admin, request }) => {
  await resetUiSettings(request);
  const serverId = await classIdByName(request, "Server");
  const recent = (await apiGet<{ data: { id: string; label: string }[] }>(request, `/configuration-items?classId=${serverId}&sort=-updatedAt&limit=1`)).data[0];
  await admin.goto("/admin/customization/layouts?class=server");
  // No designer preview any more: the class picker, and one way into the editor.
  await expect(admin.getByTestId("designer-frame")).toHaveCount(0);
  await expect(admin.getByRole("toolbar", { name: "Preview width" })).toHaveCount(0);
  await expect(admin.getByTestId("layout-status")).toContainText("Server uses the template “Standard” by default.");
  const edit = admin.getByTestId("layout-edit-ci");
  await expect(edit).toHaveText(`Edit CI: ${recent.label}`);
  let page = await openEditor(admin, edit);
  // The class's default template, whatever layout the CI shows.
  await expect(page).toHaveURL(new RegExp(`/cis/${recent.id}/layout-editor\\?template=standard$`));
  await expect(bar(page)).toContainText("Layout editor · Server");
  await expect(page.locator("[data-window]").first()).toBeVisible();
  await page.close();
  await snap(admin, "customization-layouts");

  // Another CI, found by name.
  await admin.getByLabel("Edit on another CI").fill(ci.label);
  await expect(admin.getByLabel("Configuration item to edit the layout on").locator("option", { hasText: ci.label }).first()).toBeAttached();
  await admin.getByLabel("Configuration item to edit the layout on").selectOption({ label: ci.label });
  await expect(edit).toHaveText(`Edit CI: ${ci.label}`);
  page = await openEditor(admin, edit);
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}/layout-editor\\?template=standard$`));
  await page.close();

  // A class without CIs: a link to the create form's layout editor.
  const key = `e2e_empty_${stamp}`;
  await apiSend(request, "POST", "/ci-classes", { key, name: `E2E Empty ${stamp}` });
  await admin.goto(`/admin/customization/layouts?class=${key}`);
  const create = admin.getByRole("link", { name: `Create a E2E Empty ${stamp} CI to edit its layout` });
  await expect(create).toHaveAttribute("href", /\/cis\/new\/layout-editor\?classId=[^&]+&template=standard$/);
  await expect(admin.getByTestId("layout-edit-ci")).toHaveCount(0);
  page = await openEditor(admin, create);
  await expect(page).toHaveURL(/\/cis\/new\/layout-editor\?classId=[^&]+&template=standard$/);
  await expect(bar(page)).toContainText(`Layout editor · E2E Empty ${stamp}`);
  await expect(field(page, "Ident")).toBeVisible();
  await page.close();
});

test("with popups blocked the editor opens in the same tab, and says so", async ({ page }) => {
  await page.addInitScript(() => {
    window.open = () => null;
  });
  await page.goto(`/cis/${ci.id}`);
  await page.getByRole("button", { name: "Edit layout" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}/layout-editor\\?opened=tab$`));
  await expect(bar(page).getByRole("note")).toContainText("Your browser blocked the new window");
  await expect(field(page, "Ident")).toContainText(ci.ident);
  await bar(page).getByRole("button", { name: "Done" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
  await expect(page.getByRole("tablist", { name: "CI sections" })).toBeVisible();
});

test("a layout sent on the grid (an older export or API client) opens as windows with every field", async ({ page: origin, request }) => {
  await resetUiSettings(request);
  const current = await apiGet<{ version: number }>(request, "/ui-settings");
  // Two half-width sections side by side, then one on a row of its own: the grid format before SHAA-1471.
  const fields = { general: ["ident", "validFrom", "validUntil"], side: ["attributes.serial_number", "attributes.manufacturer"], below: ["attributes.model"] };
  const grid = {
    classKey: "server",
    tabs: [
      {
        key: "general",
        label: "General",
        placement: "grid",
        sections: [
          { key: "general", label: "General", width: 6, fields: fields.general.map((field) => ({ field })) },
          { key: "side", label: "Side by side", width: 6, fields: fields.side.map((field) => ({ field })) },
          { key: "below", label: "Below", newRow: true, fields: fields.below.map((field) => ({ field })) },
        ],
      },
    ],
  };
  const res = await request.put("/api/v1/ui-settings", { data: { version: current.version, settings: { layouts: [grid] } }, headers: { "X-CSRF-Token": await csrf(request) } });
  expect(res.ok(), await res.text()).toBeTruthy();
  // Stored free, each section a window where it was on the grid. Sent without layoutFormat, the layout also gets
  // the Record and Relationships sections at the end of its first tab, where the detail page showed them.
  type Frame = { x: number; y: number; w: number; h: number; z: number };
  const stored = await apiGet<{ settings: { layouts: { classKey: string; tabs: { placement?: string; sections: { key: string; frame?: Frame }[] }[] }[] } }>(request, "/ui-settings");
  const tab = stored.settings.layouts.find((l) => l.classKey === "server")!.tabs[0];
  expect(tab.placement).toBe("free");
  expect(tab.sections.map((s) => [s.key, s.frame?.x, s.frame?.w])).toEqual([
    ["general", 0, 0.5],
    ["side", 0.5, 0.5],
    ["below", 0, 1],
    ["record", 0, 1],
    ["relations", 0, 1],
  ]);

  // The page: windows side by side, then the one below; every field there.
  await origin.goto(`/cis/${ci.id}`);
  const onPage = (key: string) => origin.locator(`.lg-free > .layout-panel[data-section="${key}"]`);
  await expect(onPage("side")).toContainText("Serial number");
  const a = (await onPage("general").boundingBox())!;
  const b = (await onPage("side").boundingBox())!;
  const c = (await onPage("below").boundingBox())!;
  expect(Math.abs(b.y - a.y)).toBeLessThan(2);
  expect(b.x).toBeGreaterThan(a.x + a.width - 1);
  expect(c.y).toBeGreaterThan(Math.max(a.y + a.height, b.y + b.height) - 1);
  // Active follows Valid until wherever that is placed.
  await expect(fieldLabels(onPage("general"))).toHaveText(["Ident", /^Valid from/, "Valid until", "Active"]);
  await expect(fieldLabels(onPage("side"))).toHaveText([/^Serial number/, /^Manufacturer/]);
  await expect(fieldLabels(onPage("below"))).toHaveText([/^Model/]);

  // The editor: the same windows, nothing to switch.
  const page = await openEditor(origin);
  for (const key of ["general", "side", "below"]) await expect(page.locator(`[data-window="${key}"]`)).toBeVisible();
  await expect(section(page, "Side by side").locator(".le-field")).toHaveCount(2);
  await expect(bar(page).getByRole("button", { name: "Grid", exact: true })).toHaveCount(0);
  await expect(bar(page).getByText("Unsaved changes")).toHaveCount(0);
  await snap(page, "layout-edit-from-grid");
  await page.close();
  await resetUiSettings(request);
});

test("content blocks: a note and built-in panels placed in the editor, on the detail page and the form", async ({ page, request }) => {
  await resetUiSettings(request);
  await page.goto(`/cis/${ci.id}/layout-editor`);
  await expect(bar(page)).toBeVisible();
  await page.setViewportSize({ width: 1440, height: 3200 });

  // + Note: added with its text editor open; limited Markdown, raw HTML shown as text.
  await page.getByRole("button", { name: "Add a note to General" }).click();
  const text = page.getByRole("textbox", { name: "Text of Note" });
  await expect(text).toBeFocused();
  await text.fill("");
  await expect(page.getByText("A note needs text.")).toBeVisible();
  await expect(section(page, "Note").getByRole("button", { name: "Apply" })).toBeDisabled();
  await text.fill("Owned by **Ops**. <b>raw</b> [Runbook](https://example.com/runbook) [bad](javascript:alert(1))\n- check backups\n- ask Ops");
  await section(page, "Note").getByRole("button", { name: "Apply" }).click();
  const note = section(page, "Note");
  await expect(note.locator("strong")).toHaveText("Ops");
  await expect(note.getByText("<b>raw</b>", { exact: false })).toBeVisible();
  await expect(note.getByRole("link", { name: "Runbook" })).toHaveAttribute("href", "https://example.com/runbook");
  await expect(note.getByRole("link", { name: "bad" })).toHaveCount(0);
  await expect(note.getByRole("listitem")).toHaveText(["check backups", "ask Ops"]);
  // Renamed like any section.
  await note.getByRole("button", { name: "Note", exact: true }).click();
  await page.getByLabel("Section name").fill("Before you edit");
  await page.keyboard.press("Enter");
  await expect(section(page, "Before you edit")).toBeVisible();

  // The relationships (placed from the start) moved to a tab of their own, + Panel adds the audit trail there;
  // each panel once per layout.
  await tabBar(page).getByRole("button", { name: "Add a tab" }).click();
  await tabBar(page).getByLabel("Tab name").fill("Links");
  await page.keyboard.press("Enter");
  await tabBar(page).getByRole("button", { name: "General", exact: true }).click();
  await section(page, "Relationships").hover();
  await section(page, "Relationships").getByLabel("Tab of Relationships").selectOption({ label: "Links" });
  await expect(tabBar(page).getByRole("button", { name: "Links", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(section(page, "Relationships")).toContainText("Relationships panel");
  await page.getByLabel("Add a panel to Links").selectOption("audit");
  await expect(section(page, "Audit trail").getByRole("columnheader", { name: "Request id" })).toBeVisible();
  await expect(page.getByLabel("Add a panel to Links").locator("option")).toHaveText(["Add panel…", "History"]);
  // The tab's empty field section goes.
  await section(page, "Links").hover();
  await section(page, "Links").getByRole("button", { name: "Remove section Links" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Remove" }).click();
  await snap(page, "layout-blocks-editor");

  // An API refusal is shown next to the block it concerns (here a refusal the editor cannot produce itself).
  await page.route("**/api/v1/ui-settings", async (route) => {
    if (route.request().method() !== "PUT") return route.fallback();
    // The editor saves the template it edits (Standard, the first template after the reset).
    const sent = route.request().postDataJSON() as { settings: { layoutTemplates: { key: string; layout: { tabs: { sections: { kind?: string }[] }[] } }[] } };
    const i = sent.settings.layoutTemplates.findIndex((x) => x.key === "standard");
    const tabs = sent.settings.layoutTemplates[i].layout.tabs;
    const t = tabs.findIndex((x) => x.sections.some((s) => s.kind === "audit"));
    const s = tabs[t].sections.findIndex((x) => x.kind === "audit");
    await route.fulfill({
      status: 400,
      json: {
        error: {
          code: "VALIDATION_ERROR",
          message: "Invalid settings",
          details: [{ field: `settings.layoutTemplates.${i}.layout.tabs.${t}.sections.${s}.kind`, message: "The audit panel can be placed once in a layout" }],
        },
      },
    });
  });
  await bar(page).getByTestId("le-save").click();
  await page.getByRole("dialog", { name: /^Save to the template/ }).getByRole("button", { name: "Save to template", exact: true }).click();
  await expect(section(page, "Audit trail").getByRole("alert")).toContainText(/settings\.layoutTemplates\.0\.layout\.tabs\.1\.sections\.\d+\.kind The audit panel can be placed once/);
  await page.unroute("**/api/v1/ui-settings");

  await saveLayout(page);
  await expect(section(page, "Audit trail").getByRole("alert")).toHaveCount(0);

  // The detail page: the note and the record details on General, the panels on Links; History keeps its tab.
  await page.goto(`/cis/${ci.id}`);
  const tabs = page.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(tabs).toHaveText(["General", "Links", "Relationship map", "Impact", "History"]);
  const heads = page.locator(".layout-container .layout-panel > .panel-header h2");
  await expect(heads).toContainText(["General", "Record", "Before you edit"]);
  await expect(page.locator(".lg-free .note-text strong")).toHaveText("Ops");
  await expect(page.locator("#rel-title")).toHaveCount(0);
  await tabs.filter({ hasText: "Links" }).click();
  await expect(heads).toHaveText(["Relationships", "Audit trail"]);
  await expect(page.getByRole("searchbox", { name: "Filter relationships" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Request id" })).toBeVisible();
  await snap(page, "layout-blocks-detail");

  // The form: the note, but no panels (a tab of panels only is left out).
  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.locator("form .note-text strong")).toHaveText("Ops");
  await expect(page.getByRole("tab", { name: "Links" })).toHaveCount(0);

});

test("windows dragged, resized, overlapped and layered, saved, and shown as placed", async ({ page: origin, request }) => {
  await resetUiSettings(request);
  await origin.goto(`/cis/${ci.id}`);
  const page = await openEditor(origin);
  await page.setViewportSize({ width: 1440, height: 3200 });
  const area = page.locator("[data-le-area]");
  const win = (key: string) => page.locator(`[data-window="${key}"]`);
  const num = async (key: string, attr: "x" | "y" | "w" | "h" | "z") => Number(await win(key).getAttribute(`data-${attr}`));
  const say = page.locator(".le-canvas [aria-live=assertive]");

  // The built-in layout's sections are windows, stacked at the full width.
  await expect(win("general")).toBeVisible();
  expect(await num("general", "x")).toBe(0);
  expect(await num("general", "w")).toBe(1);

  // + Section: a new window below the others, on top of the stack.
  await page.getByRole("button", { name: "Add a section to General" }).click();
  await page.getByLabel("Section name").fill("Floating");
  await page.getByLabel("Section name").press("Enter");
  const key = (await page.locator("[data-window]").filter({ has: section(page, "Floating") }).getAttribute("data-window"))!;
  await field(page, "Serial number").hover();
  await field(page, "Serial number").getByLabel("Move Serial number to section").selectOption({ label: "Floating" });
  await expect(section(page, "Floating").locator(".le-field")).toHaveCount(1);
  expect(await num(key, "y")).toBeGreaterThanOrEqual((await num("general", "y")) + (await num("general", "h")));
  expect(await num(key, "z")).toBeGreaterThan(await num("general", "z"));

  const areaBox = (await area.boundingBox())!;
  // Resize it from the bottom-right corner, narrower and taller, snapping to the 8 px guide grid.
  const w0 = (await win(key).boundingBox())!;
  const corner = (await win(key).getByTestId("window-edge-se").boundingBox())!;
  await page.mouse.move(corner.x + corner.width / 2, corner.y + corner.height / 2);
  await page.mouse.down();
  await page.mouse.move(corner.x - 600, corner.y + 90, { steps: 6 });
  await expect(win(key).getByTestId("window-readout")).toBeVisible();
  await page.mouse.up();
  const w1 = (await win(key).boundingBox())!;
  expect(w1.width).toBeLessThan(w0.width - 580);
  expect(w1.height).toBeGreaterThan(w0.height + 80);
  // Its right and bottom edges are on the guide grid, or on another window's edge.
  const snapped = (v: number, edges: number[]) => v % 8 === 0 || edges.some((e) => Math.abs(e - v) < 1);
  expect(snapped((await num(key, "y")) + (await num(key, "h")), [(await num("general", "y")) + (await num("general", "h"))])).toBe(true);
  // Undo takes back the whole resize; redo brings it back.
  await bar(page).getByRole("button", { name: "Undo" }).click();
  await expect.poll(async () => Math.round((await win(key).boundingBox())!.width)).toBe(Math.round(w0.width));
  await bar(page).getByRole("button", { name: "Redo" }).click();
  await expect.poll(async () => Math.round((await win(key).boundingBox())!.width)).toBe(Math.round(w1.width));

  // Drag it by its title bar over General, pixel-free with Alt: a live readout while it moves, and one undo step.
  // Grabbed by its name in the title bar (a click there renames it; a drag moves the window).
  const title = (await section(page, "Floating").getByRole("button", { name: "Floating", exact: true }).boundingBox())!;
  const from = { x: title.x + title.width / 2, y: title.y + title.height / 2 };
  const target = { x: areaBox.x + 403, y: areaBox.y + 61 };
  const box0 = (await win(key).boundingBox())!;
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.keyboard.down("Alt");
  await page.mouse.move((from.x + target.x) / 2, (from.y + target.y) / 2, { steps: 5 });
  await page.mouse.move(target.x + (from.x - box0.x), target.y + (from.y - box0.y), { steps: 5 });
  await expect(win(key).getByTestId("window-readout")).toHaveText(/^403, 61 · \d+ × \d+ px$/);
  await page.mouse.up();
  await page.keyboard.up("Alt");
  await expect(win(key).getByTestId("window-readout")).toHaveCount(0);
  expect(await num(key, "y")).toBe(61);
  expect(Math.round((await num(key, "x")) * areaBox.width)).toBe(403);
  await expect(say).toHaveText(/^Window Floating: 403, 61 · /);
  // The click that ended the drag did not rename the section.
  await expect(page.getByLabel("Section name")).toHaveCount(0);

  // Overlapping: Floating sits over General, and whatever is on top gets the pointer.
  const g = (await win("general").boundingBox())!;
  const f = (await win(key).boundingBox())!;
  const overlap = { x: Math.max(g.x, f.x) + 20, y: Math.max(g.y, f.y) + 60 };
  expect(overlap.x).toBeLessThan(Math.min(g.x + g.width, f.x + f.width));
  expect(overlap.y).toBeLessThan(Math.min(g.y + g.height, f.y + f.height));
  const onTop = () => page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.closest("[data-window]")?.getAttribute("data-window"), overlap);
  expect(await onTop()).toBe(key);
  // Pressing on General brings it to the front.
  await page.mouse.click(g.x + 12, g.y + 12);
  await expect.poll(onTop).toBe("general");
  // The built-in sections, the Record and Relationships panels, and Floating.
  await expect(bar(page).getByTestId("le-layer")).toHaveText("Layer 9 of 9");
  // The context menu sends it to the back again.
  await page.mouse.click(g.x + 12, g.y + 12, { button: "right" });
  const menu = page.getByRole("menu", { name: "Layers of General" });
  await expect(menu.getByRole("menuitem", { name: /Bring to front/ })).toBeDisabled();
  await menu.getByRole("menuitem", { name: /Send to back/ }).click();
  await expect(menu).toHaveCount(0);
  await expect.poll(onTop).toBe(key);
  await expect(say).toHaveText("Send to back: General is layer 1 of 9.");
  // …and the bar's toolbar works on the selected window: General to the front, then one step back.
  await bar(page).getByRole("button", { name: "Bring to front" }).click();
  await expect.poll(onTop).toBe("general");
  await bar(page).getByRole("button", { name: "Send backward" }).click();
  await expect.poll(onTop).toBe(key);

  // Keyboard, on Floating's grip: arrows move it (Shift: further), Ctrl+arrows resize it, Ctrl+PageDown lowers it.
  const grip = page.getByRole("button", { name: /^Window Floating: / });
  await grip.focus();
  const [x0, y0, h0] = [await num(key, "x"), await num(key, "y"), await num(key, "h")];
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Shift+ArrowDown");
  await expect.poll(() => num(key, "y")).toBe(y0 + 64);
  expect(Math.round(((await num(key, "x")) - x0) * areaBox.width)).toBe(8);
  await page.keyboard.press("Control+ArrowDown");
  await expect.poll(() => num(key, "h")).toBe(h0 + 8);
  await expect(grip).toBeFocused();
  await page.keyboard.press("Control+PageDown");
  await expect.poll(onTop).toBe("general");
  await page.keyboard.press("Control+Shift+PageUp");
  await expect.poll(onTop).toBe(key);
  await expect(grip).toBeFocused();
  // Every keyboard step is an undo step of its own.
  await page.keyboard.press("Control+z");
  await expect.poll(onTop).toBe("general");
  await page.keyboard.press("Control+Shift+z");
  await expect.poll(onTop).toBe(key);
  await snap(page, "layout-free-editor");

  const expected = { x: await num(key, "x"), y: await num(key, "y"), w: await num(key, "w"), h: await num(key, "h") };
  await saveLayout(page);
  await closeEditor(page);

  // Stored: a free tab, the frames as placed, Floating on top of General; sections in reading order (y, then x).
  type Frame = { x: number; y: number; w: number; h: number; z: number };
  // Saved to the template Server uses (Standard: the class has no default of its own).
  const stored = await apiGet<{ settings: { layoutTemplates: { key: string; layout: { tabs: { placement?: string; sections: { key: string; frame?: Frame }[] }[] } }[] } }>(request, "/ui-settings");
  const tab = stored.settings.layoutTemplates.find((t) => t.key === "standard")!.layout.tabs[0];
  expect(tab.placement).toBe("free");
  const saved = Object.fromEntries(tab.sections.map((s) => [s.key, s.frame!]));
  expect(saved[key]).toMatchObject({ y: expected.y, h: expected.h });
  expect(Math.abs(saved[key].x - expected.x)).toBeLessThan(0.0002);
  expect(saved[key].z).toBeGreaterThan(saved.general.z);
  const ys = tab.sections.map((s) => s.frame!.y);
  expect(ys).toEqual([...ys].sort((a, b) => a - b));

  // The page the editor was opened from shows the tab as placed after the save, and again after a reload.
  const onPage = (k: string) => origin.locator(`.lg-free > .layout-panel[data-section="${k}"]`);
  for (const reload of [false, true]) {
    if (reload) await origin.reload();
    await expect(onPage(key)).toContainText("Floating");
    const free = (await origin.locator(".lg-free").boundingBox())!;
    const pf = (await onPage(key).boundingBox())!;
    expect(Math.abs(pf.y - free.y - saved[key].y)).toBeLessThan(2);
    expect(Math.abs(pf.x - free.x - saved[key].x * free.width)).toBeLessThan(2);
    expect(Math.abs(pf.height - saved[key].h)).toBeLessThan(2);
    const pg = (await onPage("general").boundingBox())!;
    const point = { x: Math.max(pg.x, pf.x) + 20, y: Math.max(pg.y, pf.y) + 60 };
    expect(await origin.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.closest("[data-section]")?.getAttribute("data-section"), point)).toBe(key);
    // In the document (and for the keyboard) the windows come in reading order.
    expect(await origin.locator(".lg-free > .layout-panel").evaluateAll((els) => els.map((e) => e.getAttribute("data-section")))).toEqual(tab.sections.map((s) => s.key));
  }
  await snap(origin, "layout-free-detail");
  // On a phone the windows stack at the full width in reading order.
  await origin.setViewportSize({ width: 390, height: 844 });
  await expect(async () => {
    const a = (await onPage("general").boundingBox())!;
    const b = (await onPage(key).boundingBox())!;
    expect(b.y).toBeGreaterThan(a.y + a.height - 1);
    expect(Math.abs(b.width - a.width)).toBeLessThan(2);
  }).toPass();
  await snap(origin, "layout-free-phone");
  await origin.setViewportSize({ width: 1440, height: 900 });

  await resetUiSettings(request);
});

test("record details and relationships are panels: moved, removed and added back; separators between fields", async ({ page, request }) => {
  await resetUiSettings(request);
  await page.goto(`/cis/${ci.id}/layout-editor`);
  await expect(bar(page)).toBeVisible();
  await page.setViewportSize({ width: 1440, height: 3200 });
  const say = page.locator(".le-canvas [aria-live=assertive]");
  const panelOptions = page.getByLabel("Add a panel to General").locator("option");

  // The built-in layout shows both as windows of the first tab; nothing is pinned, and no hint says otherwise.
  await expect(section(page, "Record")).toContainText(ci.id);
  await expect(section(page, "Relationships")).toContainText("Relationships panel");
  await expect(page.getByText(/record details come last/i)).toHaveCount(0);
  await expect(panelOptions).toHaveText(["Add panel…", "History", "Audit trail"]);

  // Removed: off the detail page, and offered by + Panel again.
  await section(page, "Relationships").hover();
  await section(page, "Relationships").getByRole("button", { name: "Remove section Relationships" }).click();
  await expect(page.getByRole("dialog")).toContainText("The Relationships panel is no longer shown on the detail page.");
  await page.getByRole("dialog").getByRole("button", { name: "Remove" }).click();
  await expect(section(page, "Relationships")).toHaveCount(0);
  await expect(panelOptions).toHaveText(["Add panel…", "Relationships", "History", "Audit trail"]);

  // The record details moved to a tab of their own (its empty field section removed).
  await tabBar(page).getByRole("button", { name: "Add a tab" }).click();
  await tabBar(page).getByLabel("Tab name").fill("Meta");
  await page.keyboard.press("Enter");
  await section(page, "Meta").hover();
  await section(page, "Meta").getByRole("button", { name: "Remove section Meta" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Remove" }).click();
  await tabBar(page).getByRole("button", { name: "General", exact: true }).click();
  await section(page, "Record").hover();
  await section(page, "Record").getByLabel("Tab of Record").selectOption({ label: "Meta" });
  await expect(tabBar(page).getByRole("button", { name: "Meta", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(section(page, "Record")).toBeVisible();

  // Separators in General: added with its label, moved by keyboard and by drag, removed.
  await tabBar(page).getByRole("button", { name: "General", exact: true }).click();
  const general = section(page, "General");
  const order = () => general.locator(".le-grid > .le-field, .le-grid > .le-sep").evaluateAll((els) => els.map((e) => e.getAttribute("data-field") ?? "|"));
  await general.hover();
  await general.getByRole("button", { name: "Add a separator to General" }).click();
  await expect(page.getByLabel("Separator label (empty: a plain line)")).toBeFocused();
  await page.getByLabel("Separator label (empty: a plain line)").fill("Lifecycle");
  await page.keyboard.press("Enter");
  const sep = general.locator(".le-sep").filter({ hasText: "Lifecycle" });
  await expect(sep).toBeVisible();
  expect((await order()).at(-1)).toBe("|");
  await expect(page.getByRole("button", { name: "Separator Lifecycle, line across General" })).toBeFocused();
  await page.keyboard.press("Alt+ArrowUp");
  await expect(say).toHaveText(/^Separator moved to position \d+ of \d+ in General\.$/);
  expect((await order()).at(-2)).toBe("|");
  // A plain line, removed again with its toolbar.
  await general.hover();
  await general.getByRole("button", { name: "Add a separator to General" }).click();
  await page.keyboard.press("Enter");
  await expect(general.locator(".le-sep")).toHaveCount(2);
  await general.locator(".le-sep").last().hover();
  await general.getByRole("button", { name: "Remove Separator", exact: true }).click();
  await expect(general.locator(".le-sep")).toHaveCount(1);
  // Dragged before the field ahead of it (both in view: General's window scrolls, and a drag across that scroll is flaky).
  const ahead = (await order()).at(-3)!;
  await sep.dragTo(general.locator(`.le-grid > .le-field[data-field="${ahead}"]`), { targetPosition: { x: 4, y: 4 } });
  await expect.poll(async () => (await order()).at(-3)).toBe("|");
  expect((await order()).at(-2)).toBe(ahead);
  await snap(page, "layout-separators-editor");

  await saveLayout(page);
  type Section = { key: string; kind?: string; fields?: { field?: string; separator?: boolean; label?: string; width: number }[] };
  const stored = await apiGet<{ settings: { layoutTemplates: { key: string; layout: { tabs: { label: string; sections: Section[] }[] } }[] } }>(request, "/ui-settings");
  const tabs = stored.settings.layoutTemplates.find((t) => t.key === "standard")!.layout.tabs;
  expect(tabs.map((t) => [t.label, t.sections.filter((x) => x.kind && x.kind !== "fields").map((x) => x.kind)])).toEqual([
    ["General", []],
    ["Meta", ["record"]],
  ]);
  const generalFields = tabs[0].sections.find((x) => x.key === "general")!.fields!;
  const sepAt = generalFields.findIndex((f) => f.separator);
  expect(generalFields[sepAt]).toEqual({ separator: true, label: "Lifecycle", width: 3 });
  expect(generalFields[sepAt + 1].field).toBe(ahead);

  // The detail page: no relationships, the record details on Meta, the separator a line across General.
  await page.goto(`/cis/${ci.id}`);
  const ciTabs = page.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(ciTabs).toHaveText(["General", "Meta", "Relationship map", "Impact", "History"]);
  await expect(page.getByRole("separator", { name: "Lifecycle" })).toBeVisible();
  await expect(page.locator('.layout-panel[data-section="general"] [data-separator]')).toHaveText("Lifecycle");
  await expect(page.locator("#rel-title")).toHaveCount(0);
  await expect(page.locator(".layout-container .layout-panel > .panel-header h2", { hasText: "Record" })).toHaveCount(0);
  await ciTabs.filter({ hasText: "Meta" }).click();
  await expect(page.locator(".layout-container .layout-panel > .panel-header h2")).toHaveText(["Record"]);
  await expect(pagePanel(page, "Record")).toContainText(ci.id);
  await snap(page, "layout-separators-detail");
  // The form shows the separator too.
  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.locator("form").getByRole("separator", { name: "Lifecycle" })).toBeVisible();

  // + Panel brings the relationships back.
  await page.goto(`/cis/${ci.id}/layout-editor`);
  await expect(bar(page)).toBeVisible();
  await page.getByLabel("Add a panel to General").selectOption("relations");
  await expect(section(page, "Relationships")).toBeVisible();
  await saveLayout(page);
  await page.goto(`/cis/${ci.id}`);
  await expect(pagePanel(page, "Relationships")).toContainText("Add relationship");

  await resetUiSettings(request);
});

test("a tabbed layout upgraded by migration 0048: one Record with its details, no window cut off, and it saves with its separator", async ({ page: origin, request }) => {
  await resetUiSettings(request);
  const current = await apiGet<{ version: number }>(request, "/ui-settings");
  // What migration 0048 left (layout format 3): the record details placed at the end of the first tab, the
  // relationships on the second, each window where the old estimate (48 px per row of fields) put it (GH#619,
  // GH#621), and a separator between two fields (GH#620).
  const frame = (y: number, h: number, z: number) => ({ x: 0, y, w: 1, h, z });
  const network = [{ field: "attributes.serial_number", width: 1 }, { separator: true, label: "Vendor", width: 3 }, { field: "attributes.manufacturer", width: 1 }];
  const upgraded = {
    classKey: "server",
    tabs: [
      {
        key: "general",
        label: "General",
        placement: "free",
        sections: [
          { key: "network", label: "Network", columns: 3, width: 12, collapsed: false, fields: network, frame: frame(0, 48 + 3 * 48, 1) },
          { key: "note", label: "Read me", kind: "note", text: "Patch on Sundays", columns: 3, width: 12, collapsed: false, fields: [], frame: frame(208, 144, 2) },
          { key: "record", label: "Record", kind: "record", columns: 3, width: 12, collapsed: false, fields: [], frame: frame(368, 144, 3) },
        ],
      },
      { key: "links", label: "Links", placement: "free", sections: [{ key: "relations", label: "Relationships", kind: "relations", columns: 3, width: 12, collapsed: false, fields: [], frame: frame(0, 320, 1) }] },
    ],
    hiddenFields: [],
    readOnlyFields: [],
  };
  const res = await request.put("/api/v1/ui-settings", {
    data: { version: current.version, settings: { layoutFormat: 3, layouts: [upgraded] } },
    headers: { "X-CSRF-Token": await csrf(request) },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  // Stored in format 4: the windows the grid placed are as tall as the inline inputs need, the rest is kept.
  type Frame = { x: number; y: number; w: number; h: number; z: number };
  type Stored = { settings: { layoutFormat: number; layouts: { classKey: string; tabs: { sections: { key: string; fields?: unknown[]; frame?: Frame }[] }[] }[] } };
  const stored = await apiGet<Stored>(request, "/ui-settings");
  expect(stored.settings.layoutFormat).toBe(4);
  const tabs = stored.settings.layouts.find((l) => l.classKey === "server")!.tabs;
  expect(tabs[0].sections.map((s) => [s.key, s.frame?.y, s.frame?.h])).toEqual([
    ["network", 0, 88 + 2 * 82 + 28],
    ["note", 296, 144],
    ["record", 456, 88 + 2 * 82],
  ]);
  expect(tabs[0].sections[0].fields).toEqual(network);
  expect(tabs[1].sections.map((s) => [s.key, s.frame])).toEqual([["relations", frame(0, 320, 1)]]);

  // The detail page at a common desktop size: one Record, with the record details, where the layout puts it.
  await origin.setViewportSize({ width: 1440, height: 900 });
  await origin.goto(`/cis/${ci.id}`);
  const headings = origin.locator(".layout-container .lg-free > .lg-win > .panel-header h2");
  await expect(headings).toHaveText(["Network", "Read me", "Record"]);
  await expect(pagePanel(origin, "Record")).toContainText(ci.id);
  await expect(origin.getByRole("separator", { name: "Vendor" })).toBeVisible();
  // No window scrolls its inline inputs or the record details out of view (GH#621).
  const windows = origin.locator(".lg-free > .lg-win:not(.is-collapsed)");
  const clipped = () =>
    windows.evaluateAll((els) =>
      els.filter((e) => e.scrollHeight > e.clientHeight + 1).map((e) => `${e.getAttribute("data-section")}: ${e.clientHeight}/${e.scrollHeight}`),
    );
  await expect(origin.locator('.lg-win[data-section="network"] input').first()).toBeVisible();
  await expect(windows).toHaveCount(3);
  expect(await clipped()).toEqual([]);
  await snap(origin, "layout-upgraded-0048");
  // The comfortable density has the taller inputs the heights are sized for.
  await origin.evaluate(() => localStorage.setItem("shadoucmdb.density", "comfortable"));
  await origin.reload();
  await expect(origin.locator("html")).toHaveAttribute("data-density", "comfortable");
  await expect(origin.locator('.lg-win[data-section="network"] input').first()).toBeVisible();
  await expect(windows).toHaveCount(3);
  expect(await clipped()).toEqual([]);
  await origin.evaluate(() => localStorage.removeItem("shadoucmdb.density"));
  await origin.reload();

  // The editor loads the separator and saves a change to the layout (GH#620).
  const page = await openEditor(origin);
  await expect(section(page, "Network").locator(".le-sep")).toHaveText(/Vendor/);
  await page.getByRole("button", { name: /^Manufacturer, / }).focus();
  await page.keyboard.press("Alt+ArrowRight");
  await expect(page.getByRole("button", { name: /^Manufacturer, / })).toHaveAccessibleName(/Manufacturer, 2 of 3 columns/);
  await saveLayout(page);
  await page.close();
  type Tabs = { tabs?: { sections: { key: string; fields?: unknown[] }[] }[] };
  const saved = await apiGet<{ settings: { layouts: Tabs[]; layoutTemplates: { layout: Tabs }[] } }>(request, "/ui-settings");
  const layouts = [...saved.settings.layouts, ...saved.settings.layoutTemplates.map((t) => t.layout)];
  const fields = layouts.flatMap((l) => l.tabs ?? []).flatMap((t) => t.sections).find((s) => s.key === "network")!.fields;
  expect(fields).toEqual([network[0], network[1], { field: "attributes.manufacturer", width: 2 }]);
  await resetUiSettings(request);
});
