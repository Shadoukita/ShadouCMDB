import type { APIRequestContext } from "@playwright/test";
import { apiSend, checkA11y, chooseTheme, csrf, expect, test } from "./support";

// Imports in the reference-mockup look (design document §0, step 9c-1): the inventory's head band (breadcrumb, title
// with the count, New import, intro, All users) above the table card, mono teal file names, status pills with an
// icon, Stop or Delete in the row menu (one Stop pattern, M2) and numbered pages. Import is on only for this spec.
test.describe.configure({ mode: "serial" });

const FILE = `redesign-${Date.now().toString(36)}.csv`;
let jobId = "";

async function setImport(request: APIRequestContext, enabled: boolean) {
  await apiSend(request, "PUT", "/imports/settings", { enabled });
}

test.beforeAll(async ({ request }) => {
  await setImport(request, true);
  const res = await request.fetch("/api/v1/imports", {
    method: "POST",
    data: Buffer.from("Name;Hostname\r\nweb01;web01.example.com\r\n"),
    headers: { "Content-Type": "text/csv", "X-File-Name": FILE, "X-CSRF-Token": await csrf(request) },
  });
  expect(res.status(), "upload").toBe(202);
  jobId = ((await res.json()) as { id: string }).id;
});

test.afterAll(async ({ request }) => {
  if (jobId) await request.fetch(`/api/v1/imports/${jobId}`, { method: "DELETE", headers: { "X-CSRF-Token": await csrf(request) } });
  await setImport(request, false);
});

test("imports: head band, mono file names, status pills, row menu, numbered pages", async ({ page }, testInfo) => {
  await page.goto("/imports");
  const head = page.locator(".list-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Imports");
  await expect(head.getByRole("heading", { level: 1, name: "Imports" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d,]+ total$/);
  await expect(head.getByRole("link", { name: "New import" })).toHaveClass(/\bbtn-primary\b/);
  await expect(head.getByLabel("All users")).not.toBeChecked();

  const list = page.getByRole("region", { name: "Recent imports" });
  await expect(list.locator("table.list-table th").first()).toHaveCSS("text-transform", "none");
  const row = list.getByRole("row", { name: new RegExp(FILE.replace(".", "\\.")) });
  await expect(row.getByRole("link", { name: FILE })).toHaveClass(/\blist-name\b/);
  await expect(row.locator(".badge")).toHaveCSS("border-radius", "999px");
  await expect(row.locator(".badge svg")).toHaveCount(1);
  await expect(list.locator(".table-footer .page-numbers")).toBeVisible();

  // The row menu: Open, then Delete once the file is read (Stop while it runs); each asks first.
  await expect(row.locator(".badge")).not.toContainText(/Uploading|Reading the file|Waiting/, { timeout: 15_000 });
  await row.getByRole("button", { name: `Actions for ${FILE}` }).click();
  await expect(page.getByRole("menuitem", { name: "Open" })).toBeVisible();
  await page.getByRole("menuitem", { name: "Delete import" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete the import of “${FILE}”?` });
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await checkA11y(page, testInfo, "imports-list-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "imports-list-dark");
  await chooseTheme(page, "");
});

test("imports: the page texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto("/imports");
  const head = page.locator(".list-head");
  await expect(head.getByRole("heading", { level: 1, name: "Importe" })).toBeVisible();
  await expect(head.locator(".count")).toHaveText(/^[\d.]+ insgesamt$/);
  await expect(head.getByRole("link", { name: "Neuer Import" })).toBeVisible();
  await expect(page.getByRole("heading", { level: 2, name: "So funktioniert der Import" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: /^Datei$/ })).toBeVisible();
  await expect(page.getByRole("row", { name: new RegExp(FILE.replace(".", "\\.")) }).locator(".badge")).toHaveText(/Bereit zum Zuordnen|Datei wird gelesen|Wartet/);
});
