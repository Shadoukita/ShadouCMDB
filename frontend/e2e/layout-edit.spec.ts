import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, classIdByName, csrf, expect, resetUiSettings, snap, test } from "./support";

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
  await apiSend(request, "POST", "/admin/users", { username: VIEWER, displayName: `E2E Layout viewer ${stamp}`, password: PASSWORD, profileIds: [profile.id] });
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

/** Clicks a button that opens the layout editor, and returns its window once the editor shows. */
async function openEditor(page: Page, name: "Edit layout" | "Open on a CI" = "Edit layout"): Promise<Page> {
  const [popup] = await Promise.all([page.waitForEvent("popup"), page.getByRole("button", { name }).click()]);
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
  await page.setViewportSize({ width: 1440, height: 2400 });
  // The mode is unmistakable, and says what the change applies to.
  await expect(bar(page)).toContainText("Editing the Server layout");
  await expect(bar(page)).toContainText("Changes apply to every Server configuration item");
  await expect(bar(page).getByText("No unsaved changes")).toBeVisible();
  // The canvas is the real page: the CI's own values, the built-in General tab.
  await expect(tabBar(page).getByRole("button", { name: "General", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(field(page, "Ident")).toContainText(ci.ident);
  await expect(page.getByRole("tablist", { name: "CI sections" })).toHaveCount(0);

  // + Tab: added at the end of the tab bar, named in place.
  await tabBar(page).getByRole("button", { name: "+ Tab" }).click();
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

  // Width presets narrow the page: the grid falls back to one column.
  await bar(page).getByRole("button", { name: "Phone" }).click();
  const ident = (await field(page, "Ident").boundingBox())!;
  const from = (await field(page, "Valid from").boundingBox())!;
  expect(from.y).toBeGreaterThan(ident.y + ident.height - 1);
  await bar(page).getByRole("button", { name: "Desktop" }).click();
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
  await bar(page).getByLabel("Note for this version").fill(`e2e in-place layout ${stamp}`);
  await bar(page).getByRole("button", { name: "Save layout" }).click();
  await expect(bar(page).getByRole("status")).toContainText(/Saved as version \d+/);
  await expect(bar(page).getByText("No unsaved changes")).toBeVisible();
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
  await expect(viewer.locator(".layout-panels > details > summary h2")).toContainText(["Lifecycle"]);
  await expect(viewer.locator(".layout-panels").getByText("Asset tag", { exact: true })).toHaveCount(0);
  await expect(viewer.getByRole("button", { name: "Edit layout" })).toHaveCount(0);
  await vtabs.filter({ hasText: "Hardware" }).click();
  await expect(viewer.locator(".layout-panels > details > summary h2")).toHaveText(["Hardware facts"]);
  await expect(viewer.locator(".layout-panels dt")).toHaveText(["Model", "CPU cores"]);
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
  await expect(bar(page)).toContainText("Editing the Server layout");
  // The form's own controls, inert, with the CI's values.
  await expect(field(page, "Ident").locator("#f-ident")).toHaveValue(ci.ident);
  await expect(page.getByRole("button", { name: "Save changes" })).toHaveCount(0);
  // The field's toolbar shows while it is hovered (or holds the keyboard focus).
  await field(page, "Serial number").hover();
  await field(page, "Serial number").getByLabel("Read-only").check();
  await expect(field(page, "Serial number")).toContainText("read-only");
  await bar(page).getByRole("button", { name: "Save layout" }).click();
  await expect(bar(page).getByRole("status")).toContainText(/Saved as version \d+/);
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
  await expect(bar(page).getByText("No unsaved changes")).toBeVisible();
  // Redo resets again; Discard goes back to the saved layout.
  await bar(page).getByRole("button", { name: "Redo" }).click();
  await expect(bar(page).getByText("Unsaved changes")).toBeVisible();
  await bar(page).getByRole("button", { name: "Discard" }).click();
  await expect(tabBar(page).getByRole("button", { name: "Hardware", exact: true })).toBeVisible();
  await expect(bar(page).getByText("No unsaved changes")).toBeVisible();
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
  await bar(page).getByRole("button", { name: "Save layout" }).click();
  await expect(bar(page).getByRole("alert").filter({ hasText: "Someone else saved the settings" })).toBeVisible();
  await bar(page).getByRole("button", { name: "Load the latest version" }).click();
  await expect(bar(page).getByText("No unsaved changes")).toBeVisible();
  await expect(bar(page).getByRole("alert")).toHaveCount(0);
});

test("the designer opens its class on a real CI, or on an empty form without CIs", async ({ page: designer, request }) => {
  await designer.goto("/admin/customization/layouts?class=server");
  let page = await openEditor(designer, "Open on a CI");
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}/layout-editor$`));
  await expect(bar(page)).toContainText("Editing the Server layout");
  await page.close();

  const key = `e2e_empty_${stamp}`;
  await apiSend(request, "POST", "/ci-classes", { key, name: `E2E Empty ${stamp}` });
  await designer.goto(`/admin/customization/layouts?class=${key}`);
  page = await openEditor(designer, "Open on a CI");
  await expect(page).toHaveURL(/\/cis\/new\/layout-editor\?classId=[^&]+$/);
  await expect(bar(page)).toContainText(`Editing the E2E Empty ${stamp} layout`);
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

test("sections side by side: resize by dragging, drop beside another, saved, stacked on a phone", async ({ page: origin, request }) => {
  await resetUiSettings(request);
  await origin.goto(`/cis/${ci.id}`);
  const page = await openEditor(origin);
  const shell = (key: string) => page.locator(`[data-section-shell="${key}"]`);
  const grid = page.getByRole("region", { name: "Tab General" });
  const say = page.locator(".le-canvas [aria-live=assertive]");

  // Drag the General section's right edge to the middle of the tab: it snaps to 6 of 12 columns, with a live guide.
  const edge = (await shell("general").getByTestId("section-edge-right").boundingBox())!;
  const box = (await grid.boundingBox())!;
  await page.mouse.move(edge.x + edge.width / 2, edge.y + edge.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * 0.75, edge.y + edge.height / 2, { steps: 4 });
  await page.mouse.move(box.x + box.width / 2 + 3, edge.y + edge.height / 2, { steps: 6 });
  await expect(shell("general").getByTestId("section-guide")).toHaveText("6 / 12");
  await page.mouse.up();
  await expect(shell("general")).toHaveAttribute("data-width", "6");
  await expect(shell("general").getByTestId("section-guide")).toHaveCount(0);
  await expect(say).toHaveText("Section General: 6 of 12 columns wide.");
  // The whole drag is one undo step.
  await bar(page).getByRole("button", { name: "Undo" }).click();
  await expect(shell("general")).toHaveAttribute("data-width", "12");
  await expect(bar(page).getByText("No unsaved changes")).toBeVisible();
  await bar(page).getByRole("button", { name: "Redo" }).click();
  await expect(shell("general")).toHaveAttribute("data-width", "6");

  // A new section at the end of the tab, with a field in it.
  await page.getByRole("button", { name: "Add a section to General" }).click();
  await page.getByLabel("Section name").fill("Side by side");
  await page.getByLabel("Section name").press("Enter");
  await field(page, "Serial number").hover();
  await field(page, "Serial number").getByLabel("Move Serial number to section").selectOption({ label: "Side by side" });
  await expect(section(page, "Side by side").locator(".le-field")).toHaveCount(1);
  // Renaming keeps the key the section got when it was added.
  const key = (await page.locator("[data-section-shell]").filter({ has: section(page, "Side by side") }).getAttribute("data-section-shell"))!;
  const grip = page.getByRole("button", { name: /^Section Side by side, / });

  // Alt+↑ on its grip moves it up, to right below General.
  await grip.focus();
  const shells = page.locator("[data-section-shell]");
  for (let i = 0; i < 10 && (await shells.nth(1).getAttribute("data-section-shell")) !== key; i++) await page.keyboard.press("Alt+ArrowUp");
  await expect(shells.nth(1)).toHaveAttribute("data-section-shell", key);
  await expect(grip).toBeFocused();

  // Drag it by its grip onto General's right edge: it sits next to General, sharing the row.
  await grip.scrollIntoViewIfNeeded();
  const general = (await shell("general").boundingBox())!;
  await grip.dragTo(shell("general"), { targetPosition: { x: general.width - 10, y: general.height - 40 } });
  await expect(say).toHaveText("Section Side by side placed right of General, 6 of 12 columns wide.");
  await expect(shell(key)).toHaveAttribute("data-width", "6");
  const g = (await shell("general").boundingBox())!;
  const n = (await shell(key).boundingBox())!;
  expect(Math.abs(n.y - g.y)).toBeLessThan(2);
  expect(n.x).toBeGreaterThan(g.x + g.width - 1);

  // Keyboard: Alt+← / Alt+→ on the section's grip resize it by one column.
  await grip.focus();
  await page.keyboard.press("Alt+ArrowLeft");
  await expect(shell(key)).toHaveAttribute("data-width", "5");
  await page.keyboard.press("Alt+ArrowRight");
  await expect(shell(key)).toHaveAttribute("data-width", "6");
  await snap(page, "layout-edit-side-by-side");

  await bar(page).getByRole("button", { name: "Save layout" }).click();
  await expect(bar(page).getByRole("status")).toContainText(/Saved as version \d+/);
  await closeEditor(page);

  // The CI page shows the two sections side by side on a wide screen…
  const onPage = (key: string) => origin.locator(`.layout-panels > details[data-section="${key}"]`);
  await expect(onPage(key)).toContainText("Serial number");
  const a = (await onPage("general").boundingBox())!;
  const b = (await onPage(key).boundingBox())!;
  expect(Math.abs(b.y - a.y)).toBeLessThan(2);
  expect(b.x).toBeGreaterThan(a.x + a.width - 1);
  expect(Math.abs(b.width - a.width)).toBeLessThan(2);
  await snap(origin, "layout-side-by-side-detail");
  // …and stacked at the full width on a phone.
  await origin.setViewportSize({ width: 390, height: 844 });
  await expect(async () => {
    const pa = (await onPage("general").boundingBox())!;
    const pb = (await onPage(key).boundingBox())!;
    expect(pb.y).toBeGreaterThan(pa.y + pa.height - 1);
    expect(Math.abs(pb.width - pa.width)).toBeLessThan(2);
  }).toPass();
  await snap(origin, "layout-side-by-side-phone");
  await origin.setViewportSize({ width: 1440, height: 900 });
  await resetUiSettings(request);
});

test("content blocks: a note and built-in panels placed in the editor, on the detail page, the form and in the designer", async ({ page, request }) => {
  await resetUiSettings(request);
  await page.goto(`/cis/${ci.id}/layout-editor`);
  await expect(bar(page)).toBeVisible();
  await page.setViewportSize({ width: 1440, height: 2400 });

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

  // + Panel: the relationships and the audit trail on a tab of their own; each panel once per layout.
  await tabBar(page).getByRole("button", { name: "+ Tab" }).click();
  await tabBar(page).getByLabel("Tab name").fill("Links");
  await page.keyboard.press("Enter");
  await page.getByLabel("Add a panel to Links").selectOption("relations");
  await expect(section(page, "Relationships")).toContainText("Relationships panel");
  await page.getByLabel("Add a panel to Links").selectOption("audit");
  await expect(section(page, "Audit trail").getByRole("columnheader", { name: "Request id" })).toBeVisible();
  await expect(page.getByLabel("Add a panel to Links").locator("option")).toHaveText(["+ Panel", "History"]);
  // The tab's empty field section goes.
  await section(page, "Links").hover();
  await section(page, "Links").getByRole("button", { name: "Remove section Links" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Remove" }).click();
  await snap(page, "layout-blocks-editor");

  // An API refusal is shown next to the block it concerns (here a refusal the editor cannot produce itself).
  await page.route("**/api/v1/ui-settings", async (route) => {
    if (route.request().method() !== "PUT") return route.fallback();
    const sent = route.request().postDataJSON() as { settings: { layouts: { tabs: { sections: { kind?: string }[] }[] }[] } };
    const t = sent.settings.layouts[0].tabs.findIndex((x) => x.sections.some((s) => s.kind === "audit"));
    const s = sent.settings.layouts[0].tabs[t].sections.findIndex((x) => x.kind === "audit");
    await route.fulfill({
      status: 400,
      json: { error: { code: "VALIDATION_ERROR", message: "Invalid settings", details: [{ field: `settings.layouts.0.tabs.${t}.sections.${s}.kind`, message: "The audit panel can be placed once in a layout" }] } },
    });
  });
  await bar(page).getByRole("button", { name: "Save layout" }).click();
  await expect(section(page, "Audit trail").getByRole("alert")).toContainText(/settings\.layouts\.0\.tabs\.1\.sections\.\d+\.kind The audit panel can be placed once/);
  await page.unroute("**/api/v1/ui-settings");

  await bar(page).getByRole("button", { name: "Save layout" }).click();
  await expect(bar(page).getByRole("status")).toContainText(/Saved as version \d+/);
  await expect(section(page, "Audit trail").getByRole("alert")).toHaveCount(0);

  // The detail page: the note on General, the panels on Links instead of their usual places; History keeps its tab.
  await page.goto(`/cis/${ci.id}`);
  const tabs = page.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(tabs).toHaveText(["General", "Links", "Relationship map", "Impact", "History"]);
  const heads = page.locator(".layout-panels > details > summary h2");
  await expect(heads).toContainText(["General", "Before you edit", "Record"]);
  await expect(page.locator(".layout-panels .note-text strong")).toHaveText("Ops");
  await expect(page.locator("#rel-title")).toHaveCount(0);
  await tabs.filter({ hasText: "Links" }).click();
  await expect(heads).toHaveText(["Relationships", "Audit trail"]);
  await expect(page.getByRole("columnheader", { name: "Related CI" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Request id" })).toBeVisible();
  await snap(page, "layout-blocks-detail");

  // The form: the note, but no panels (a tab of panels only is left out).
  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.locator("form .note-text strong")).toHaveText("Ops");
  await expect(page.getByRole("tab", { name: "Links" })).toHaveCount(0);

  // The designer draws the blocks; clearing the note's text is refused by the API, next to the note.
  await page.goto("/admin/customization/layouts?class=server");
  const d = (label: string) => page.getByRole("region", { name: `Section ${label}`, exact: true });
  await expect(d("Before you edit").locator(".note-text strong")).toHaveText("Ops");
  await d("Before you edit").getByRole("button", { name: "Before you edit" }).click();
  await page.getByLabel("Text", { exact: true }).fill("");
  await expect(page.getByText("A note needs text.")).toBeVisible();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(d("Before you edit").getByRole("alert")).toContainText(/settings\.layouts\.0\.tabs\.0\.sections\.\d+\.text Must not be blank/);
  await page.getByRole("button", { name: "Discard", exact: true }).click();
  await page.getByRole("tab", { name: "Links" }).click();
  await expect(d("Relationships")).toContainText("Relationships panel");
  await expect(d("Audit trail")).toContainText("Audit trail panel");
  await snap(page, "layout-blocks-designer");
});

test("free placement: windows dragged, resized, overlapped and layered, saved, and shown as placed", async ({ page: origin, request }) => {
  await resetUiSettings(request);
  await origin.goto(`/cis/${ci.id}`);
  const page = await openEditor(origin);
  await page.setViewportSize({ width: 1440, height: 1600 });
  const area = page.locator("[data-le-area]");
  const win = (key: string) => page.locator(`[data-window="${key}"]`);
  const num = async (key: string, attr: "x" | "y" | "w" | "h" | "z") => Number(await win(key).getAttribute(`data-${attr}`));
  const say = page.locator(".le-canvas [aria-live=assertive]");

  // Grid stays the default; switching the tab to Free keeps every section where it is.
  const placement = bar(page).getByRole("group", { name: "Placement of the tab General" });
  await expect(placement.getByRole("button", { name: "Grid" })).toHaveAttribute("aria-pressed", "true");
  const before = (await page.locator('[data-section-shell="general"]').boundingBox())!;
  await placement.getByRole("button", { name: "Free" }).click();
  await expect(area).toHaveAttribute("data-placement", "free");
  const after = (await win("general").boundingBox())!;
  expect(Math.abs(after.y - before.y)).toBeLessThan(2);
  expect(Math.abs(after.x - before.x)).toBeLessThan(2);
  expect(Math.abs(after.width - before.width)).toBeLessThan(2);
  expect(Math.abs(after.height - before.height)).toBeLessThan(2);
  await expect(say).toContainText("Tab General is free");

  // + Section on a free tab: a new window below the others, on top of the stack.
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
  await expect(bar(page).getByTestId("le-layer")).toHaveText("Layer 7 of 7");
  // The context menu sends it to the back again.
  await page.mouse.click(g.x + 12, g.y + 12, { button: "right" });
  const menu = page.getByRole("menu", { name: "Layers of General" });
  await expect(menu.getByRole("menuitem", { name: /Bring to front/ })).toBeDisabled();
  await menu.getByRole("menuitem", { name: /Send to back/ }).click();
  await expect(menu).toHaveCount(0);
  await expect.poll(onTop).toBe(key);
  await expect(say).toHaveText("Send to back: General is layer 1 of 7.");
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
  await bar(page).getByRole("button", { name: "Save layout" }).click();
  await expect(bar(page).getByRole("status")).toContainText(/Saved as version \d+/);
  await closeEditor(page);

  // Stored: a free tab, the frames as placed, Floating on top of General; sections in reading order (y, then x).
  type Frame = { x: number; y: number; w: number; h: number; z: number };
  const stored = await apiGet<{ settings: { layouts: { classKey: string; tabs: { placement?: string; sections: { key: string; frame?: Frame }[] }[] }[] } }>(request, "/ui-settings");
  const tab = stored.settings.layouts.find((l) => l.classKey === "server")!.tabs[0];
  expect(tab.placement).toBe("free");
  const saved = Object.fromEntries(tab.sections.map((s) => [s.key, s.frame!]));
  expect(saved[key]).toMatchObject({ y: expected.y, h: expected.h });
  expect(Math.abs(saved[key].x - expected.x)).toBeLessThan(0.0002);
  expect(saved[key].z).toBeGreaterThan(saved.general.z);
  const ys = tab.sections.map((s) => s.frame!.y);
  expect(ys).toEqual([...ys].sort((a, b) => a - b));

  // The page the editor was opened from shows the tab as placed after the save, and again after a reload.
  const onPage = (k: string) => origin.locator(`.lg-free > details[data-section="${k}"]`);
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
    expect(await origin.locator(".lg-free > details").evaluateAll((els) => els.map((e) => e.getAttribute("data-section")))).toEqual(tab.sections.map((s) => s.key));
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

  // Back to the grid: the editor orders the sections by position, as the API does.
  const editor = await openEditor(origin);
  await bar(editor).getByRole("group", { name: "Placement of the tab General" }).getByRole("button", { name: "Grid" }).click();
  await expect(editor.locator("[data-le-area]")).toHaveAttribute("data-placement", "grid");
  expect(await editor.locator("[data-section-shell]").evaluateAll((els) => els.map((e) => e.getAttribute("data-section-shell")))).toEqual(tab.sections.map((s) => s.key));
  await expect(editor.locator("[data-window]")).toHaveCount(0);
  await resetUiSettings(request);
});
