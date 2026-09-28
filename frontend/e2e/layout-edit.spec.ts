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
  await expect(tabs).toHaveText(["General", "Hardware", "Relationship map", "History"]);
  // Done closes the editor's window.
  await closeEditor(page);

  // The version history has the note.
  const versions = await apiGet<{ data: { comment: string | null }[] }>(origin.request, "/ui-settings/versions?limit=1");
  expect(versions.data[0].comment).toBe(`e2e in-place layout ${stamp}`);

  // A user who may only view servers sees the new layout, and no way to edit it.
  const viewer = await signInUi(browser, VIEWER, PASSWORD);
  await viewer.goto(`/cis/${ci.id}`);
  const vtabs = viewer.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(vtabs).toHaveText(["General", "Hardware", "Relationship map"]);
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
