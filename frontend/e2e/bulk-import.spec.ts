import { readFile } from "node:fs/promises";
import AxeBuilder from "@axe-core/playwright";
import type { APIRequestContext, Browser, Page, TestInfo } from "@playwright/test";
import { apiGet, apiSend, csrf, expect, test } from "./support";

// Bulk import (spec SHAA-714 §7.3): the instance switch, who sees the entry points, and the wizard. The switch is
// off after installation and this spec turns it off again at the end, so the other specs see the stock instance.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PROFILE = `E2E importers ${stamp}`;
const NO_IMPORT_PROFILE = `E2E no import ${stamp}`;
const IMPORTER = `e2e-importer-${stamp}`;
const PLAIN = `e2e-plain-${stamp}`;
const PASSWORD = "importer-password-123";
const FAILING = new Set(["critical", "serious"]);

async function setImport(request: APIRequestContext, enabled: boolean) {
  await apiSend(request, "PUT", "/imports/settings", { enabled });
}

async function signInUi(browser: Browser, username: string): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  return page;
}

async function checkA11y(page: Page, testInfo: TestInfo, name: string) {
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    const results = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"]).analyze();
    await testInfo.attach(`axe-${name}-${colorScheme}.json`, { body: JSON.stringify(results.violations, null, 2), contentType: "application/json" });
    const failing = results.violations.filter((v) => FAILING.has(v.impact ?? "")).map((v) => `[${v.impact}] ${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
    expect(failing, `critical/serious WCAG 2.1 AA violations on ${name} (${colorScheme})`).toEqual([]);
  }
  await page.emulateMedia({ colorScheme: null });
}

const CSV = ["Name;Hostname;Status", "web01;web01.example.com;Im Betrieb", "Müller-DB;db01.example.com;In Wartung", "app01;app01.example.com;"].join("\r\n");

test.beforeAll(async ({ request }) => {
  const importers = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: PROFILE,
    globalPermissions: ["cis.import"],
    classPermissions: [{ classId: null, view: true, create: true, edit: true, delete: false }],
  });
  const plain = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: NO_IMPORT_PROFILE,
    globalPermissions: [],
    classPermissions: [{ classId: null, view: true, create: true, edit: true, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: IMPORTER, displayName: `E2E Importer ${stamp}`, password: PASSWORD, profileIds: [importers.id] });
  await apiSend(request, "POST", "/admin/users", { username: PLAIN, displayName: `E2E Plain ${stamp}`, password: PASSWORD, profileIds: [plain.id] });
  await setImport(request, false);
});

test.afterAll(async ({ request }) => {
  await setImport(request, false);
});

test("off by default: no entry points, and /imports says so", async ({ browser }) => {
  const page = await signInUi(browser, IMPORTER);
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByRole("link", { name: "Import", exact: true })).toHaveCount(0);
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Bulk import" })).toHaveCount(0);
  await page.goto("/imports");
  await expect(page.getByText("Bulk import is turned off for this instance.")).toBeVisible();
  await expect(page.getByRole("link", { name: "+ New import" })).toHaveCount(0);
  await page.context().close();
});

test("the administrator turns import on under Administration › Import, and the audit log records it", async ({ page, request }) => {
  const before = (await apiGet<{ page: { total: number } }>(request, "/audit-log?entityType=import_settings&limit=1")).page.total;
  await page.goto("/admin/import");
  const toggle = page.getByLabel("Bulk import enabled");
  await expect(toggle).not.toBeChecked();
  await toggle.check();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Bulk import is turned on.")).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Bulk import enabled")).toBeChecked();
  await expect(page.getByText("Largest file")).toBeVisible();
  expect((await apiGet<{ page: { total: number } }>(request, "/audit-log?entityType=import_settings&limit=1")).page.total).toBe(before + 1);
});

test("cis.import appears in the profile editor", async ({ page }) => {
  await page.goto("/admin/profiles/new");
  await expect(page.getByText("Bulk import", { exact: true })).toBeVisible();
  await expect(page.getByText("Import configuration items from CSV and Excel files (still limited by the class rights)")).toBeVisible();
});

test("without cis.import there are no entry points and /imports is permission denied", async ({ browser }) => {
  const page = await signInUi(browser, PLAIN);
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByRole("link", { name: "Import", exact: true })).toHaveCount(0);
  await page.goto("/imports");
  await expect(page.getByRole("heading", { name: "Permission denied" })).toBeVisible();
  await expect(page.getByText("You need the Bulk import permission.")).toBeVisible();
  await page.context().close();
});

test("upload a CSV: step 1 reads it, shows the preview, and a reload resumes there", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, IMPORTER);
  await page.goto("/cis");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Bulk import" }).click();
  await expect(page).toHaveURL(/\/imports$/);
  await expect(page.getByRole("heading", { name: "No imports yet" })).toBeVisible();
  await checkA11y(page, testInfo, "imports-empty");

  // The server writes the template: one header row, Ident plus the class's attribute labels.
  await page.getByLabel("Template for class").selectOption({ label: "Server" });
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Download template (CSV)" }).click()]);
  expect(download.suggestedFilename()).toBe("server-template.csv");
  const template = await readFile(await download.path(), "utf-8");
  expect(template).toContain("Ident");
  await page.getByRole("link", { name: "+ New import" }).first().click();
  await expect(page).toHaveURL(/\/imports\/new$/);
  await expect(page.getByRole("list").filter({ hasText: "Upload" }).locator('[aria-current="step"]')).toHaveText(/Upload/);
  await checkA11y(page, testInfo, "import-upload");

  // Refused in the browser before any upload.
  const fileInput = page.getByLabel(/Spreadsheet file \(CSV or XLSX/);
  await fileInput.setInputFiles({ name: "old.xls", mimeType: "application/vnd.ms-excel", buffer: Buffer.from("x") });
  await expect(page.getByRole("alert")).toContainText("Save it as .xlsx");
  await expect(fileInput).toHaveAttribute("aria-invalid", "true");

  await fileInput.setInputFiles({ name: `servers-${stamp}.csv`, mimeType: "text/csv", buffer: Buffer.from(CSV, "utf-8") });
  await expect(page).toHaveURL(/\/imports\/[0-9a-f-]{36}$/);
  await expect(page.getByRole("heading", { level: 1, name: `servers-${stamp}.csv` })).toBeVisible();
  await expect(page.getByText("3 data rows, 3 columns")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByLabel("Delimiter")).toHaveValue(";");
  const preview = page.locator("table", { has: page.locator("caption", { hasText: "rows as read" }) });
  await expect(preview.getByRole("columnheader", { name: "Hostname" })).toBeVisible();
  await expect(preview.getByRole("cell", { name: "Müller-DB" })).toBeVisible();
  await checkA11y(page, testInfo, "import-file-read");

  // A reload opens the same step, with nothing lost.
  await page.reload();
  await expect(page.getByText("3 data rows, 3 columns")).toBeVisible();
  await expect(page.locator('[aria-current="step"]')).toContainText("Upload");

  // Without a header row the columns are named by letter and the first row becomes data.
  await page.getByLabel("The first row contains column names").uncheck();
  await page.getByRole("button", { name: "Read the file again" }).click();
  await expect(page.getByText("4 data rows, 3 columns")).toBeVisible({ timeout: 20_000 });
  await page.getByLabel("The first row contains column names").check();
  await page.getByRole("button", { name: "Read the file again" }).click();
  await expect(page.getByText("3 data rows, 3 columns")).toBeVisible({ timeout: 20_000 });

  await page.getByRole("button", { name: "Next: Map columns" }).click();
  await expect(page.locator('[aria-current="step"]')).toContainText("Map columns");
  await expect(page.getByRole("heading", { level: 2, name: "Map columns" })).toBeFocused();
  await page.context().close();
});

test("another user's import, or none at all, reads the same", async ({ page }) => {
  await page.goto("/imports/00000000-0000-4000-8000-000000000000");
  await expect(page.getByText("This import does not exist or belongs to another user.")).toBeVisible();
});

test("while import is off, the user still sees and deletes their import", async ({ browser, request }) => {
  await setImport(request, false);
  const page = await signInUi(browser, IMPORTER);
  await page.goto("/imports");
  await expect(page.getByText("Bulk import is turned off for this instance.")).toBeVisible();
  await expect(page.getByText("Your remaining imports are listed below")).toBeVisible();
  const row = page.getByRole("row", { name: new RegExp(`servers-${stamp}\\.csv`) });
  await expect(row).toBeVisible();
  await row.getByRole("button", { name: /Delete import of/ }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Configuration items already imported are not changed");
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await dialog.getByRole("button", { name: "Delete import" }).click();
  await expect(page.getByRole("heading", { name: "No imports yet" })).toBeVisible();
  await page.context().close();
  // The server refuses a new upload while import is off.
  const res = await request.fetch("/api/v1/imports", {
    method: "POST",
    data: Buffer.from(CSV),
    headers: { "Content-Type": "text/csv", "X-File-Name": "x.csv", "X-CSRF-Token": await csrf(request) },
  });
  expect(res.status()).toBe(403);
});
