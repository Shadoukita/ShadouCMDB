import type { APIRequestContext, Locator, Page } from "@playwright/test";
import { apiGet, classIdByName, createCi, csrf, expect, resetUiSettings, snap, test } from "./support";

// Layout templates (SHAA-1473): a template made in Customization › Layouts, laid out in the layout editor,
// made the Server default, and shown by a Server CI created afterwards; then one CI's own layout, saved
// for that CI only and reset to the class default. Starts and ends with the built-in settings, and removes
// the CIs and their layouts it made, so the other specs see the stock UI.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const TEMPLATE = `E2E hosts ${stamp}`;
const TAB = `Runbook ${stamp}`;
const made: string[] = [];
let serverId = "";

test.beforeAll(async ({ request }) => {
  await resetUiSettings(request);
  serverId = await classIdByName(request, "Server");
});
test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  for (const id of made) {
    // A CI's layout first: a template a live CI uses cannot be removed by the reset.
    await request.delete(`/api/v1/configuration-items/${id}/layout`, { headers });
    await request.delete(`/api/v1/configuration-items/${id}`, { headers });
  }
  await resetUiSettings(request);
});

/** A section of the detail page by its heading. */
const pagePanel = (page: Page, heading: string) => page.locator(".layout-container details").filter({ has: page.locator("summary h2", { hasText: new RegExp(`^${heading}$`) }) });
const bar = (page: Page) => page.getByRole("region", { name: "Layout editing" });
const tabBar = (page: Page) => page.getByRole("group", { name: "Tabs of the layout" });
const templates = (page: Page) => page.getByTestId("layout-templates");
const templateRow = (page: Page, name: string) => templates(page).getByRole("row").filter({ has: page.getByRole("rowheader", { name: new RegExp(`^${name}`) }) });
const saveBar = (page: Page) => page.getByRole("region", { name: "Save changes" });
const ciTabs = (page: Page) => page.getByRole("tablist", { name: "CI sections" }).getByRole("tab");

async function saveCustomization(page: Page) {
  await saveBar(page).getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: /Saved as version \d+/ })).toBeVisible();
}

/** Clicks what opens the layout editor and returns its window once the editor shows. */
async function openEditor(page: Page, opener: Locator): Promise<Page> {
  const [popup] = await Promise.all([page.waitForEvent("popup"), opener.click()]);
  await expect(bar(popup)).toBeVisible();
  await popup.setViewportSize({ width: 1440, height: 2000 });
  return popup;
}

async function closeEditor(page: Page) {
  const closed = page.waitForEvent("close");
  await bar(page).getByRole("button", { name: "Done" }).click().catch(() => undefined);
  await closed;
}

/** A note with `text` on the tab in view of the editor. */
async function addNote(page: Page, tab: string, text: string) {
  await page.getByRole("button", { name: `Add a note to ${tab}` }).click();
  await page.getByRole("textbox", { name: "Text of Note" }).fill(text);
  await page.getByRole("region", { name: "Section Note", exact: true }).getByRole("button", { name: "Apply" }).click();
}

const ciLayout = (request: APIRequestContext, id: string) =>
  apiGet<{ source: string; templateKey: string | null; templateName: string | null }>(request, `/configuration-items/${id}/layout`);

test("a new template, laid out in the editor and made the class default, is what a new CI shows", async ({ page, request }) => {
  await page.goto("/admin/customization/layouts");
  // Both tables: the classes with their default template, the templates with who uses them.
  await expect(page.getByTestId("layout-classes")).toBeVisible();
  await expect(templateRow(page, "Standard")).toContainText("built-in");

  // New template, blank: in the draft until the customization is saved; no editor before that.
  await page.getByRole("button", { name: "New template" }).click();
  const dialog = page.getByRole("dialog", { name: "New layout template" });
  await dialog.getByRole("button", { name: "Add template" }).click();
  await expect(dialog.getByText("Enter a name")).toBeVisible();
  await dialog.getByLabel("Name").fill("standard");
  await dialog.getByRole("button", { name: "Add template" }).click();
  await expect(dialog.getByText("Another template has this name")).toBeVisible();
  await dialog.getByLabel("Name").fill(TEMPLATE);
  await dialog.getByLabel("Description").fill("Servers with a runbook tab");
  await expect(dialog.getByLabel("Start from")).toHaveValue("");
  await dialog.getByRole("button", { name: "Add template" }).click();
  const row = templateRow(page, TEMPLATE);
  await expect(row).toContainText("not saved yet");
  await expect(row.getByRole("button", { name: `Edit the template ${TEMPLATE}` })).toBeDisabled();
  await saveCustomization(page);
  await expect(row).not.toContainText("not saved yet");
  await expect(row.getByRole("cell").nth(1)).toHaveText("0");
  // Not in use: it can be deleted (the Delete button is live); Standard cannot.
  await expect(row.getByRole("button", { name: `Delete the template ${TEMPLATE}` })).not.toHaveAttribute("aria-disabled", "true");
  await expect(templateRow(page, "Standard").getByRole("button", { name: "Delete the template Standard" })).toHaveAttribute("aria-disabled", "true");

  // Edit: the layout editor on a CI, on this template; a tab with a note, saved to the template.
  const editor = await openEditor(page, row.getByRole("button", { name: `Edit the template ${TEMPLATE}` }));
  await expect(editor).toHaveURL(/\/layout-editor\?template=e2e_hosts_/);
  await expect(bar(editor).getByTestId("le-target")).toHaveText(`Template: ${TEMPLATE} (used by 0 classes, 0 CIs)`);
  // A blank template has the record details and the relationships from the start, as windows to move or remove.
  await expect(editor.getByRole("region", { name: "Section Record", exact: true })).toBeVisible();
  await expect(editor.getByRole("region", { name: "Section Relationships", exact: true })).toBeVisible();
  await expect(editor.getByLabel("Add a panel to General").locator("option")).toHaveText(["+ Panel", "History", "Audit trail"]);
  await tabBar(editor).getByRole("button", { name: "+ Tab" }).click();
  await tabBar(editor).getByLabel("Tab name").fill(TAB);
  await editor.keyboard.press("Enter");
  await addNote(editor, TAB, "Restart order: app, then database.");
  await bar(editor).getByTestId("le-save").click();
  const confirm = editor.getByRole("dialog", { name: `Save to the template “${TEMPLATE}”?` });
  await expect(confirm.getByTestId("le-impact")).toHaveText("No class uses this template by default, and no CI shows it instead of its class's default. All of them show the change.");
  await confirm.getByLabel("Note for this version").fill(`e2e template ${stamp}`);
  await confirm.getByRole("button", { name: "Save to template", exact: true }).click();
  await expect(bar(editor).getByRole("status")).toContainText(`Saved to the template “${TEMPLATE}”`);
  await closeEditor(editor);

  // The template becomes the Server default (inline select, saved with the page).
  await page.getByLabel("Search", { exact: true }).fill("Server");
  const select = page.getByLabel("Default template of Server", { exact: true });
  await expect(select).toHaveValue("standard");
  await select.selectOption({ label: TEMPLATE });
  await saveCustomization(page);
  await expect(row.getByRole("cell").nth(1)).toHaveText("1");
  // In use now: Delete is refused, and says by whom.
  const del = row.getByRole("button", { name: `Delete the template ${TEMPLATE}` });
  await expect(del).toHaveAttribute("aria-disabled", "true");
  await expect(del).toHaveAttribute("title", /In use by 1 class \(Server\)/);
  // The filter by template is in the URL.
  await page.getByLabel("Default template", { exact: true }).selectOption({ label: TEMPLATE });
  await expect(page).toHaveURL(/uses=e2e_hosts_/);
  await page.reload();
  await expect(page.getByTestId("layout-classes").getByRole("rowheader")).toHaveText([/^Server/]);
  await snap(page, "layout-templates-admin");

  // A Server CI created now shows the template: its tab and its note; no "own layout" marker.
  const ci = await createCi(request, serverId, `e2e-tpl-${stamp}`);
  made.push(ci.id);
  await page.goto(`/cis/${ci.id}`);
  await expect(ciTabs(page)).toContainText([TAB]);
  await ciTabs(page).filter({ hasText: TAB }).click();
  await expect(page.getByText("Restart order: app, then database.")).toBeVisible();
  await expect(page.getByTestId("ci-own-layout")).toHaveCount(0);
  // The template's General tab: the record details and the relationships, as placed.
  await ciTabs(page).filter({ hasText: "General" }).click();
  await expect(pagePanel(page, "Record")).toContainText(ci.id);
  await expect(pagePanel(page, "Relationships")).toContainText("No relationships yet");
  expect(await ciLayout(request, ci.id)).toMatchObject({ source: "class_default", templateName: TEMPLATE });
});

test("one CI's own layout: saved for this CI only, marked on its page, and reset to the class default", async ({ page, request }) => {
  const ci = await createCi(request, serverId, `e2e-own-${stamp}`);
  const other = await createCi(request, serverId, `e2e-other-${stamp}`);
  made.push(ci.id, other.id);
  await page.goto(`/cis/${ci.id}`);
  let editor = await openEditor(page, page.getByRole("button", { name: "Edit layout" }));
  // The CI shows its class's default: that template is what the editor edits; the CI has nothing to reset.
  await expect(bar(editor).getByTestId("le-target")).toContainText(`Template: ${TEMPLATE} (used by 1 class`);
  await expect(bar(editor).getByRole("button", { name: "Reset to class default" })).toHaveCount(0);
  await addNote(editor, "General", "Only this CI: patched by hand.");
  await bar(editor).getByRole("button", { name: "More ways to save" }).click();
  await editor.getByRole("menuitem", { name: "Save for this CI only" }).click();
  await expect(bar(editor).getByRole("status")).toHaveText("Saved for this CI only.");
  await expect(bar(editor).getByTestId("le-target")).toHaveText("This CI only");
  await expect(bar(editor).getByTestId("le-save")).toHaveText("Save for this CI only");
  await expect(bar(editor).getByRole("button", { name: "Reset to class default" })).toBeVisible();
  await snap(editor, "layout-templates-ci-only");
  await closeEditor(editor);

  // The page it was opened from follows, and marks the CI's own layout (for those who may change layouts).
  await expect(page.getByText("Only this CI: patched by hand.")).toBeVisible();
  await expect(page.getByTestId("ci-own-layout")).toHaveText("Own layout");
  expect((await ciLayout(request, ci.id)).source).toBe("custom");
  // Another CI of the class is untouched.
  await page.goto(`/cis/${other.id}`);
  await expect(ciTabs(page)).toContainText([TAB]);
  await expect(page.getByText("Only this CI: patched by hand.")).toHaveCount(0);

  // Customization › Layouts counts it for Server (SHAA-1514), and the count lists exactly that CI.
  await page.goto("/admin/customization/layouts?q=Server");
  const serverRow = page.getByTestId("layout-classes").getByRole("row").filter({ has: page.getByRole("rowheader", { name: /^Server server/ }) });
  const count = serverRow.getByTestId("own-layout-count");
  await expect(count).toHaveText("1");
  await count.getByRole("link").click();
  await expect(page).toHaveURL(/\/cis\?.*ownLayout=true/);
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}.*includeSubclasses=false|includeSubclasses=false.*classId=${serverId}`));
  await expect(page.getByTestId("filter-own-layout")).toContainText("Own layout");
  await expect(page.locator(".pagination")).toContainText("1–1 of 1");
  await expect(page.getByRole("link", { name: `e2e-own-${stamp}`, exact: true })).toBeVisible();
  await expect(page.getByRole("link", { name: `e2e-other-${stamp}`, exact: true })).toHaveCount(0);

  // Use template…: the CI shows another template (Standard), marked with its name.
  await page.goto(`/cis/${ci.id}`);
  editor = await openEditor(page, page.getByRole("button", { name: "Edit layout" }));
  await expect(bar(editor).getByTestId("le-target")).toHaveText("This CI only");
  await bar(editor).getByRole("button", { name: "Use template…" }).click();
  const use = editor.getByRole("dialog", { name: "Use another template for this CI" });
  await use.getByLabel("Template").selectOption({ label: "Standard" });
  await use.getByRole("button", { name: "Use template" }).click();
  await expect(bar(editor).getByRole("status")).toHaveText("This CI now shows the template “Standard”.");
  await expect(bar(editor).getByTestId("le-target")).toContainText("Template: Standard");
  await expect(page.getByTestId("ci-own-layout")).toHaveText("Layout: Standard");
  await expect(ciTabs(page)).not.toContainText([TAB]);

  // Reset to class default (confirmed): the class's template again, no marker.
  await bar(editor).getByRole("button", { name: "Reset to class default" }).click();
  const reset = editor.getByRole("dialog", { name: "Reset this CI to its class's default layout?" });
  await expect(reset).toContainText(`stops showing the template “Standard” and shows the Server default template “${TEMPLATE}” again`);
  await reset.getByRole("button", { name: "Reset to class default" }).click();
  await expect(bar(editor).getByRole("status")).toHaveText(`This CI shows its class's default template “${TEMPLATE}” again.`);
  await expect(bar(editor).getByRole("button", { name: "Reset to class default" })).toHaveCount(0);
  await closeEditor(editor);
  await expect(page.getByTestId("ci-own-layout")).toHaveCount(0);
  await expect(ciTabs(page)).toContainText([TAB]);
  await expect(page.getByText("Only this CI: patched by hand.")).toHaveCount(0);
  expect((await ciLayout(request, ci.id)).source).toBe("class_default");
});

test("save as a new template from the editor, made the class default", async ({ page, request }) => {
  const ci = await createCi(request, serverId, `e2e-new-${stamp}`);
  made.push(ci.id);
  await page.goto(`/cis/${ci.id}`);
  const editor = await openEditor(page, page.getByRole("button", { name: "Edit layout" }));
  await addNote(editor, "General", "Saved as a template of its own.");
  await bar(editor).getByRole("button", { name: "More ways to save" }).click();
  await editor.getByRole("menuitem", { name: "Save as new template…" }).click();
  const dialog = editor.getByRole("dialog", { name: "Save as a new template" });
  await dialog.getByLabel("Name").fill(TEMPLATE);
  await dialog.getByRole("button", { name: "Save as template" }).click();
  await expect(dialog.getByText("Another template has this name")).toBeVisible();
  await dialog.getByLabel("Name").fill(`${TEMPLATE} v2`);
  await dialog.getByLabel("Make it the default for Server").check();
  await dialog.getByRole("button", { name: "Save as template" }).click();
  await expect(bar(editor).getByRole("status")).toContainText(`Saved as the new template “${TEMPLATE} v2”`);
  await expect(bar(editor).getByTestId("le-target")).toContainText(`Template: ${TEMPLATE} v2 (used by 1 class`);
  await closeEditor(editor);
  expect(await ciLayout(request, ci.id)).toMatchObject({ source: "class_default", templateName: `${TEMPLATE} v2` });
  await expect(page.getByText("Saved as a template of its own.")).toBeVisible();
});
