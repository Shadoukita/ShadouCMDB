import type { Page } from "@playwright/test";
import { classIdByName, createCi, csrf, expect, resetUiSettings, test } from "./support";

// The inventory's Columns popover and the `columns` URL parameter, and the search page's filters:
// the URL holds the whole list state, so reload, a bookmark and Back/Forward show the same list.
// Runs on the built-in UI settings (no list views) against the demo seed's Server and Application classes.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
// "zz-…" sorts after the demo CIs: other specs open the first Server by label.
const tag = `zz-e2e-cols-${stamp}`;
const created: string[] = [];
let serverId = "";
let applicationId = "";

const DEFAULTS = ["Label", "Ident", "Class", "Active", "Updated"];
const headers = (page: Page) => page.locator("table.data thead th:not(.row-actions)");
const headerTexts = (names: string[]) => names.map((h) => new RegExp(`^\\s*${h}`));
const popover = (page: Page) => page.getByRole("dialog", { name: "Columns" });

test.beforeAll(async ({ request }) => {
  await resetUiSettings(request);
  serverId = await classIdByName(request, "Server");
  applicationId = await classIdByName(request, "Application");
  created.push((await createCi(request, serverId, `${tag}-server`, { hostname: `host-${stamp}` })).id);
  created.push((await createCi(request, applicationId, `${tag}-app`)).id);
});

test.afterAll(async ({ request }) => {
  const h = { "X-CSRF-Token": await csrf(request) };
  for (const id of created) expect((await request.delete(`/api/v1/configuration-items/${id}`, { headers: h })).status()).toBe(204);
});

test("adding a column keeps the default columns and puts the choice in the URL (GH#168)", async ({ page }) => {
  await page.goto(`/cis?classId=${serverId}`);
  await expect(headers(page)).toHaveText(headerTexts(DEFAULTS));
  await page.getByRole("button", { name: /^Columns/ }).click();
  const shown = popover(page).getByRole("list", { name: "Shown columns" }).getByRole("listitem");
  await expect(shown).toHaveText(headerTexts(DEFAULTS));
  await popover(page).getByRole("group", { name: "Attributes of Server" }).getByLabel("Hostname").click(); // not check(): the box moves to "Shown columns"
  await expect(page).toHaveURL(/columns=label,ident,class,active,updatedAt,attributes\.hostname/);
  await expect(headers(page)).toHaveText(headerTexts([...DEFAULTS, "Hostname"]));
  await expect(page.getByRole("button", { name: /^Columns/ })).toContainText("Custom");
});

test("reorder, remove, reload, Back and Forward, and Reset", async ({ page }) => {
  await page.goto(`/cis?classId=${serverId}&columns=label,ident,class,active,updatedAt,attributes.hostname`);
  await page.getByRole("button", { name: /^Columns/ }).click();
  await popover(page).getByRole("button", { name: "Move Hostname up" }).click();
  await expect(page).toHaveURL(/columns=label,ident,class,active,attributes\.hostname,updatedAt/);
  // The moved row keeps the focus, so the operator can press it again.
  await expect(popover(page).getByRole("button", { name: "Move Hostname up" })).toBeFocused();
  await popover(page).getByLabel("Class", { exact: true }).click(); // not uncheck(): the box moves to "More fields"
  const after = ["Label", "Ident", "Active", "Hostname", "Updated"];
  await expect(headers(page)).toHaveText(headerTexts(after));
  // Unticked, Class moves to "More fields" and keeps the focus there.
  await expect(popover(page).getByRole("group", { name: "More fields" }).getByLabel("Class", { exact: true })).toBeFocused();

  await page.reload();
  await expect(headers(page)).toHaveText(headerTexts(after));
  await page.goBack();
  await expect(headers(page)).toHaveText(headerTexts(["Label", "Ident", "Class", "Active", "Hostname", "Updated"]));
  await page.goForward();
  await expect(headers(page)).toHaveText(headerTexts(after));

  await page.getByRole("button", { name: /^Columns/ }).click();
  await popover(page).getByRole("button", { name: "Reset to default columns" }).click();
  await expect(page).not.toHaveURL(/columns=/);
  await expect(headers(page)).toHaveText(headerTexts(DEFAULTS));
});

test("a bookmark restores rows, columns and sort; another class drops the attribute columns", async ({ page, browser }) => {
  const url = `/cis?classId=${serverId}&q=${tag}&sort=-attributes.hostname&columns=label,attributes.hostname,ident`;
  // A new browser page, as a bookmark opened later.
  const fresh = await browser.newPage({ storageState: await page.context().storageState() });
  await fresh.goto(url);
  await expect(headers(fresh)).toHaveText(headerTexts(["Label", "Hostname", "Ident"]));
  await expect(fresh.getByRole("columnheader", { name: /Hostname/ })).toHaveAttribute("aria-sort", "descending");
  await expect(fresh.locator("table.data tbody tr")).toHaveCount(1);
  await expect(fresh.locator("table.data tbody tr").first()).toContainText(`host-${stamp}`);

  await fresh.getByLabel("Class", { exact: true }).selectOption({ label: "Application" });
  await expect(fresh).toHaveURL(/columns=label,ident(&|$)/);
  await expect(fresh).not.toHaveURL(/sort=/);
  await expect(headers(fresh)).toHaveText(headerTexts(["Label", "Ident"]));
  await fresh.close();
});

test("an attribute column needs one class: without it the popover says so and the column is not shown", async ({ page }) => {
  await page.goto(`/cis?columns=label,attributes.hostname,updatedAt`);
  await expect(headers(page)).toHaveText(headerTexts(["Label", "Updated"]));
  await page.getByRole("button", { name: /^Columns/ }).click();
  await expect(popover(page).getByText("Filter by one class to add its attributes as columns.")).toBeVisible();
});

test("the Columns popover works with the keyboard alone", async ({ page }) => {
  await page.goto(`/cis?classId=${serverId}`);
  await expect(headers(page)).toHaveText(headerTexts(DEFAULTS));
  const button = page.getByRole("button", { name: /^Columns/ });
  await button.focus();
  await page.keyboard.press("Enter");
  await expect(button).toHaveAttribute("aria-expanded", "true");
  // Label is always shown (its box is disabled): the first box to use is Ident's.
  const ident = popover(page).getByLabel("Ident", { exact: true });
  await expect(ident).toBeFocused();
  await page.keyboard.press("Space");
  await expect(headers(page)).toHaveText(headerTexts(["Label", "Class", "Active", "Updated"]));
  await expect(ident).toBeFocused();
  await page.keyboard.press("Space");
  await expect(headers(page)).toHaveText(headerTexts(["Label", "Class", "Active", "Updated", "Ident"]));
  await page.keyboard.press("Escape");
  await expect(popover(page)).toHaveCount(0);
  await expect(button).toBeFocused();
  // Tabbing out closes it too.
  await page.keyboard.press("Enter");
  await expect(popover(page)).toBeVisible();
  await popover(page).getByRole("button", { name: "Done" }).focus();
  await page.keyboard.press("Tab");
  await expect(popover(page)).toHaveCount(0);
});

test("search: class, validity and deleted filters live in the URL and carry over to the inventory", async ({ page }) => {
  await page.goto(`/search?q=${tag}`);
  const results = page.locator("table.data tbody tr");
  await expect(results).toHaveCount(2);
  const filters = page.getByRole("group", { name: "Filter the results" });
  await filters.getByLabel("Class").selectOption({ label: "Server" });
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(results).toHaveCount(1);
  await expect(results.first()).toContainText(`${tag}-server`);
  await filters.getByLabel("Deleted CIs").selectOption("include");
  await expect(page).toHaveURL(/deleted=include/);

  await page.reload();
  await expect(results).toHaveCount(1);
  await expect(page.getByRole("group", { name: "Filter the results" }).getByLabel("Class")).toHaveValue(serverId);
  await page.goBack();
  await expect(page).not.toHaveURL(/deleted=/);
  await expect(results).toHaveCount(1);
  await page.goBack();
  await expect(results).toHaveCount(2);
  await page.goForward();
  await expect(results).toHaveCount(1);

  await page.getByRole("link", { name: "Open as filterable inventory" }).click();
  await expect(page).toHaveURL(/\/cis\?/);
  await expect(page).toHaveURL(new RegExp(`classId=${serverId}`));
  await expect(page.locator("table.data tbody tr")).toHaveCount(1);

  await page.goto(`/search?q=${tag}&classId=${applicationId}&active=all`);
  await expect(results).toHaveCount(1);
  await page.getByRole("group", { name: "Filter the results" }).getByRole("button", { name: "Clear filters" }).click();
  await expect(page).toHaveURL(new RegExp(`/search\\?q=${tag}$`));
  await expect(results).toHaveCount(2);
});
