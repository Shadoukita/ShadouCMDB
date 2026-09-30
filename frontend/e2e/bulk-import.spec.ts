import { readFile } from "node:fs/promises";
import AxeBuilder from "@axe-core/playwright";
import type { APIRequestContext, Browser, Page, TestInfo } from "@playwright/test";
import { apiGet, apiSend, ciIdByName, classIdByName, createCi, csrf, expect, test } from "./support";
import { xlsx } from "./xlsx";

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

/** Saved mappings this spec leaves behind (also from an earlier, interrupted run): with two of the same headers none is auto-applied. */
async function removeSavedMappings(request: APIRequestContext) {
  const list = await apiGet<{ data: { id: string; name: string; version: number }[] }>(request, "/import-mappings");
  for (const m of list.data.filter((m) => m.name.startsWith("Vendor export "))) {
    const res = await request.fetch(`/api/v1/import-mappings/${m.id}?version=${m.version}`, { method: "DELETE", headers: { "X-CSRF-Token": await csrf(request) } });
    expect(res.status(), `DELETE mapping ${m.name}`).toBe(204);
  }
}

test.afterAll(async ({ request }) => {
  await removeSavedMappings(request);
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

// ---------- Steps 2–4 against the demo data model (§7.3 cases 2–12) ----------
// Everything is looked up by the class's own labels, as an operator would pick them: nothing here depends on a
// frontend list of classes or fields.

const APP = (n: number) => `app-${stamp}-${String(n).padStart(2, "0")}`;
const DB = `db-${stamp}`;
const SRV_A = `srv-a-${stamp}`;
const SRV_B = `srv-b-${stamp}`;
const VENDOR = `Vendor export ${stamp}`;
const HEADER = "Name;Status;Version;Primary database;Runs on";
// Several relationship targets share one cell, separated by ";" like the file's delimiter, so the cell is quoted.
const appRow = (n: number, version = "1.0", status = "In service", db = DB, runsOn = n % 2 ? `${SRV_A};${SRV_B}` : SRV_A) =>
  [APP(n), status, version, db, `"${runsOn}"`].join(";");
const appsCsv = (rows: string[]) => Buffer.from([HEADER, ...rows].join("\r\n"), "utf-8");
const range = (n: number) => Array.from({ length: n }, (_, i) => i + 1);

async function newImport(page: Page, file: { name: string; mimeType: string; buffer: Buffer }, query = "") {
  await page.goto(`/imports/new${query}`);
  await page.getByLabel(/Spreadsheet file \(CSV or XLSX/).setInputFiles(file);
  await expect(page).toHaveURL(/\/imports\/[0-9a-f-]{36}/);
  await expect(page.getByText(/\d+ data rows?, \d+ columns/)).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Next: Map columns" }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Map columns" })).toBeFocused();
  return page.url().match(/\/imports\/([0-9a-f-]{36})/)![1]!;
}

/** Waits for the server's auto-match of the chosen class to fill the table. */
async function mappingReady(page: Page, columns: number) {
  await expect(page.getByRole("table").locator("caption", { hasText: `${columns} columns in the file` })).toBeVisible({ timeout: 20_000 });
  await expect(page.getByText("Matching the file's columns…")).toHaveCount(0);
}

async function checkFile(page: Page) {
  await page.getByRole("button", { name: "Next: Check file" }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Check" })).toBeVisible();
  await expect(page.getByRole("list", { name: "Check result" })).toBeVisible({ timeout: 30_000 });
}

const counts = (page: Page) => page.getByRole("list", { name: "Check result" });
/** The visible result line of step 4 (a live region repeats it for screen readers). */
const result = (page: Page) => page.locator("p.import-result");

test("happy path, CSV: auto-matched columns with a lookup, a reference and a relationship; check; import", async ({ browser, request }, testInfo) => {
  await setImport(request, true);
  await removeSavedMappings(request);
  const dbClass = await classIdByName(request, "Database");
  const serverClass = await classIdByName(request, "Server");
  await createCi(request, dbClass, DB, { engine: "postgresql" });
  await createCi(request, serverClass, SRV_A);
  await createCi(request, serverClass, SRV_B);

  const page = await signInUi(browser, IMPORTER);
  await newImport(page, { name: `apps-${stamp}.csv`, mimeType: "text/csv", buffer: appsCsv(range(20).map((n) => appRow(n))) });
  await checkA11y(page, testInfo, "import-mapping-no-class");

  await page.getByLabel("Target class").selectOption({ label: "Application" });
  await mappingReady(page, 5);
  // Auto-match (§3.3): keys and labels of the class's attributes and relationship types.
  await expect(page.getByLabel("Name", { exact: true })).toHaveValue("attr:name");
  await expect(page.getByLabel("Status", { exact: true })).toHaveValue("attr:status");
  await expect(page.getByLabel("Primary database", { exact: true })).toHaveValue("attr:primary_database");
  await expect(page.getByLabel("Runs on", { exact: true })).toHaveValue(/^rel:runs_on:/);
  await expect(page.getByRole("cell", { name: /Matched by key/ }).first()).toBeVisible();
  await expect(page.getByRole("cell", { name: /Matched by name/ }).first()).toBeVisible();

  // Without a match key the mapping is refused before anything is sent, with the problem named in the summary.
  await page.getByLabel("Match existing CIs by").selectOption("");
  await page.getByRole("button", { name: "Next: Check file" }).click();
  const summary = page.getByRole("alert").filter({ hasText: "problem in the mapping" });
  await expect(summary).toBeFocused();
  await expect(summary).toContainText("Choose how rows find existing CIs");
  await summary.getByRole("link").first().click();
  await expect(page.getByLabel("Match existing CIs by")).toBeFocused();
  await checkA11y(page, testInfo, "import-mapping-errors");

  // Mapped twice: shown on the row and in the summary.
  await page.getByLabel("Match existing CIs by").selectOption({ label: "Name" });
  await page.getByLabel("Version", { exact: true }).selectOption("attr:name");
  await page.getByRole("button", { name: "Next: Check file" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "problem in the mapping" })).toContainText("Mapped twice");
  await expect(page.getByLabel("Version", { exact: true })).toHaveAttribute("aria-invalid", "true");
  await page.getByLabel("Version", { exact: true }).selectOption("attr:version");

  // Save the layout for the next file with these headers (§1.2, D9).
  await page.getByRole("button", { name: "Save mapping as…" }).click();
  const save = page.getByRole("dialog", { name: "Save mapping as" });
  await expect(save).toBeVisible();
  await save.getByLabel("Name").fill(VENDOR);
  await save.getByLabel("Description (optional)").fill("Application list from the vendor portal");
  await checkA11y(page, testInfo, "import-save-mapping-dialog");
  await save.getByRole("button", { name: "Save mapping" }).click();
  await expect(page.getByText(`Mapping saved as “${VENDOR}”.`)).toBeVisible();
  await expect(page.getByLabel("Saved mapping")).toHaveValue(/[0-9a-f-]{36}/);

  // At 320 CSS px the table becomes one card per column (1.4.10).
  await page.setViewportSize({ width: 320, height: 800 });
  await checkA11y(page, testInfo, "import-mapping-320");
  await page.setViewportSize({ width: 1440, height: 900 });

  await checkFile(page);
  await expect(counts(page)).toContainText("Create 20");
  await expect(counts(page)).toContainText("Errors 0 rows");
  await expect(counts(page)).toContainText("Relationships to add 30");
  await expect(page.locator("caption", { hasText: "Planned changes: all 20 rows that create, update or fail" })).toBeVisible();
  await checkA11y(page, testInfo, "import-check-clean");

  await page.reload();
  await expect(page.locator('[aria-current="step"]')).toContainText("Check");
  await expect(counts(page)).toContainText("Create 20");

  await page.getByRole("button", { name: "Import 20 rows" }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Import" })).toBeVisible();
  await expect(result(page)).toContainText("Import finished: 20 created, 0 updated, 0 unchanged, 30 relationships added.", { timeout: 30_000 });
  await expect(page.locator('[aria-current="step"]')).toContainText("Import");
  await checkA11y(page, testInfo, "import-done");

  await page.reload();
  await expect(result(page)).toContainText("Import finished: 20 created");

  await page.getByRole("link", { name: "Open inventory" }).click();
  await expect(page).toHaveURL(/\/cis\?classId=/);
  await page.context().close();

  const imported = await apiGet<{ id: string; attributes: Record<string, unknown> }>(request, `/configuration-items/${await ciIdByName(request, APP(1))}`);
  expect(imported.attributes.version).toBe("1.0");
  const audit = await apiGet<{ page: { total: number } }>(request, "/audit-log?action=import.commit&limit=1");
  expect(audit.page.total).toBeGreaterThan(0);
});

test("saved mapping and update: the same headers apply the mapping; 2 changed cells update, 18 stay unchanged", async ({ browser }) => {
  const page = await signInUi(browser, IMPORTER);
  const rows = range(20).map((n) => appRow(n, n === 3 || n === 7 ? "2.0" : "1.0"));
  await newImport(page, { name: `apps-${stamp}-v2.csv`, mimeType: "text/csv", buffer: appsCsv(rows) }, "?classKey=application");
  await mappingReady(page, 5);
  await expect(page.getByText(`Mapping ${VENDOR} applied because the column names match.`)).toBeVisible();
  await expect(page.getByLabel("Match existing CIs by")).toHaveValue("attributes.name");
  await expect(page.getByRole("cell", { name: /From saved mapping/ }).first()).toBeVisible();

  await checkFile(page);
  await expect(counts(page)).toContainText("Update 2");
  await expect(counts(page)).toContainText("Unchanged 18");
  await expect(page.getByRole("cell", { name: /Version: 1\.0 → 2\.0/ }).first()).toBeVisible();
  await page.getByRole("button", { name: "Import 20 rows" }).click();
  await expect(result(page)).toContainText("Import finished: 0 created, 2 updated, 18 unchanged, 0 relationships added.", { timeout: 30_000 });
  await page.context().close();
});

test("errors and report: per-row problems, the neutralised report, a corrected file, and skipping the rest", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, IMPORTER);
  const bad = [
    appRow(21, "1.0", "Nope"), // unknown lookup value
    appRow(22, "=cmd|' /C calc'!A0"), // stays a text, but the report neutralises it
    appRow(23, "1.0", ""), // missing required value
    appRow(24),
    appRow(24, "1.1"), // duplicate key within the file
    appRow(25, "1.0", "In service", `missing-db-${stamp}`), // reference to no CI
  ];
  const firstId = await newImport(page, { name: `apps-${stamp}-bad.csv`, mimeType: "text/csv", buffer: appsCsv(bad) }, "?classKey=application");
  await mappingReady(page, 5);
  await checkFile(page);
  await expect(counts(page)).not.toContainText("Errors 0 rows");
  const problems = page.getByRole("table", { name: "Row problems" });
  await expect(problems.getByRole("row", { name: /^2 / })).toContainText("Status");
  await expect(problems.getByRole("row", { name: /^4 / })).toBeVisible();
  await expect(problems.getByRole("row", { name: /^7 / })).toContainText("Primary database");
  await checkA11y(page, testInfo, "import-check-errors");

  // Filters live in the URL, so a reload shows the same page.
  await page.getByLabel("Column").selectOption({ label: "B · Status" });
  await expect(page).toHaveURL(/issueColumn=1/);
  await page.reload();
  await expect(page.getByLabel("Column")).toHaveValue("1");
  await expect(problems.getByRole("row", { name: /^7 / })).toHaveCount(0);
  await page.getByRole("button", { name: "Clear filters" }).click();

  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Download error report" }).click()]);
  expect(download.suggestedFilename()).toMatch(/-errors\.csv$/);
  const report = (await readFile(await download.path(), "utf-8")).replace(/^﻿/, "");
  const [head, ...lines] = report.split("\r\n").filter(Boolean);
  expect(head).toMatch(/^"Row"[,;]"Severity"[,;]"Column"[,;]"Problem"[,;]"Code"[,;]"Name"/);
  expect(lines.every((l) => l.startsWith('"'))).toBeTruthy();
  expect(report).not.toContain(`"=cmd`);

  // Upload a corrected file: a new job with the same class and mapping. One row still has an error.
  await page.getByRole("link", { name: "Upload a corrected file" }).click();
  await expect(page).toHaveURL(new RegExp(`/imports/new\\?classKey=application&fromJob=${firstId}`));
  const fixed = [appRow(21), appRow(22, "1.0"), appRow(23), appRow(24), appRow(25, "1.0", "In service", `missing-db-${stamp}`)];
  await page.getByLabel(/Spreadsheet file \(CSV or XLSX/).setInputFiles({ name: `apps-${stamp}-fixed.csv`, mimeType: "text/csv", buffer: appsCsv(fixed) });
  await expect(page.getByText("5 data rows, 5 columns")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Next: Map columns" }).click();
  await expect(page.getByText("The mapping of the previous upload was applied")).toBeVisible();
  await expect(page.getByLabel("Target class")).toHaveValue("application");
  await expect(page.getByLabel("Runs on", { exact: true })).toHaveValue(/^rel:runs_on:/);
  await checkFile(page);
  await expect(counts(page)).toContainText("Create 4");
  await expect(counts(page)).toContainText("Errors 1 row");

  // Skip: the confirmation says exactly what happens, starts on Cancel, and returns focus when closed.
  const skip = page.getByRole("button", { name: "Import 4 valid rows and skip 1…" });
  await skip.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Skip 1 row with errors?" });
  await expect(dialog).toContainText("1 row has errors and will not be imported. They are listed in the error report. The other 4 rows will be imported.");
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await checkA11y(page, testInfo, "import-skip-dialog");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(skip).toBeFocused();
  await page.keyboard.press("Enter");
  await dialog.getByRole("button", { name: "Import 4 rows" }).click();
  await expect(result(page)).toContainText("Import finished: 4 created, 0 updated, 0 unchanged, 1 skipped", { timeout: 30_000 });
  const [skipped] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Download error report" }).click()]);
  expect(await readFile(await skipped.path(), "utf-8")).toContain(APP(25));
  await page.context().close();
});

test("XLSX: sheet choice, typed dates and numbers, and a relationship column with several targets", async ({ browser, request }) => {
  const host = (n: number) => `xsrv-${stamp}-${n}`;
  const workbook = xlsx([
    { name: "Read me", rows: [["This sheet is not data"]] },
    {
      name: "Servers",
      rows: [
        ["Name", "Status", "Hostname", "CPU cores", "Memory (GB)", "Purchase date", "Hosts"],
        [host(1), "In service", `${host(1)}.example.com`, 16, 64.5, new Date(Date.UTC(2024, 2, 15)), `${APP(1)};${APP(2)}`],
        [host(2), "in_service", `${host(2)}.example.com`, 8, 32, new Date(Date.UTC(2023, 10, 1)), APP(3)],
      ],
    },
    { name: "Hidden", rows: [["x"]], hidden: true },
  ]);
  const page = await signInUi(browser, IMPORTER);
  await page.goto("/imports/new?classKey=server");
  await page.getByLabel(/Spreadsheet file \(CSV or XLSX/).setInputFiles({ name: `servers-${stamp}.xlsx`, mimeType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", buffer: workbook });
  await expect(page.getByLabel("Sheet")).toBeVisible({ timeout: 20_000 });
  await page.getByLabel("Sheet").selectOption("Servers");
  await page.getByRole("button", { name: "Read the file again" }).click();
  await expect(page.getByText("2 data rows, 7 columns")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Next: Map columns" }).click();
  await mappingReady(page, 7);
  await expect(page.getByLabel("Hosts", { exact: true })).toHaveValue(/^rel:runs_on:/);
  await page.getByLabel("Match existing CIs by").selectOption({ label: "Hostname" });
  await checkFile(page);
  await expect(counts(page)).toContainText("Create 2");
  await expect(counts(page)).toContainText("Relationships to add 3");
  await page.getByRole("button", { name: "Import 2 rows" }).click();
  await expect(result(page)).toContainText("Import finished: 2 created", { timeout: 30_000 });
  await page.context().close();
  const ci = await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${await ciIdByName(request, host(1))}`);
  expect(ci.attributes).toMatchObject({ purchase_date: "2024-03-15", cpu_cores: 16, memory_gb: 64.5 });
});

test("a user with rights on one class can only pick that class", async ({ browser, request }) => {
  const serverClass = await classIdByName(request, "Server");
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E server importers ${stamp}`,
    globalPermissions: ["cis.import"],
    classPermissions: [{ classId: serverClass, view: true, create: true, edit: true, delete: false }],
  });
  const user = `e2e-srv-importer-${stamp}`;
  await apiSend(request, "POST", "/admin/users", { username: user, displayName: `E2E Server importer ${stamp}`, password: PASSWORD, profileIds: [profile.id] });
  const page = await signInUi(browser, user);
  await newImport(page, { name: `one-class-${stamp}.csv`, mimeType: "text/csv", buffer: appsCsv([appRow(1)]) });
  const classes = await page.getByLabel("Target class").locator("option").allTextContents();
  expect(classes).toContain("Server");
  expect(classes).not.toContain("Virtual machine");
  expect(classes).not.toContain("Application");
  await page.context().close();
});

test("stale check: a data model change asks for a new check, and a failing poll shows the retry banner", async ({ browser, request }) => {
  const page = await signInUi(browser, IMPORTER);
  const id = await newImport(page, { name: `apps-${stamp}-stale.csv`, mimeType: "text/csv", buffer: appsCsv([appRow(1)]) }, "?classKey=application");
  await mappingReady(page, 5);
  await checkFile(page);
  await expect(counts(page)).toContainText("Unchanged 1");

  // An administrator changes an attribute of the class after the check.
  const appClass = await classIdByName(request, "Application");
  const attrs = await apiGet<{ data: { id: string; key: string; helpText: string | null }[] }>(request, `/ci-classes/${appClass}/attributes`);
  const version = attrs.data.find((a) => a.key === "version")!;
  await apiSend(request, "PATCH", `/attribute-definitions/${version.id}`, { helpText: `Changed by e2e ${stamp}` });
  await page.reload();
  await expect(page.getByText("This check is out of date.")).toBeVisible();
  await expect(page.getByRole("button", { name: /^Import/ })).toHaveCount(0);

  // The server keeps checking while polls fail; the page says so and recovers.
  let failing = true;
  await page.route(`**/api/v1/imports/${id}`, (route) =>
    failing && route.request().method() === "GET"
      ? route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: { code: "INTERNAL_ERROR", message: "boom", requestId: "e2e", details: [] } }) })
      : route.continue(),
  );
  await page.getByRole("button", { name: "Check again" }).click();
  await expect(page.getByText("Lost connection to the server, retrying…")).toBeVisible({ timeout: 10_000 });
  failing = false;
  await expect(page.getByText("Lost connection to the server, retrying…")).toBeHidden({ timeout: 40_000 });
  await expect(page.getByRole("button", { name: "Import 1 row" })).toBeVisible({ timeout: 20_000 });
  await page.context().close();
});

test("stop: the confirmation states what happens, with counts, and starts on Cancel", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, IMPORTER);
  await page.goto("/imports");
  const done = page.getByRole("row", { name: new RegExp(`apps-${stamp}\\.csv`) });
  await done.getByRole("link").first().click();
  await expect(result(page)).toContainText("Import finished: 20 created");
  const id = page.url().match(/\/imports\/([0-9a-f-]{36})/)![1]!;
  // Replay the job as if the import were still running: 1,500 of 4,000 rows done.
  await page.route(`**/api/v1/imports/${id}`, async (route) => {
    const res = await route.fetch();
    const job = await res.json();
    await route.fulfill({
      response: res,
      json: { ...job, status: "committing", phase: "commit", finishedAt: null, progress: { ...job.progress, done: 1500, total: 4000, startedAt: new Date().toISOString() } },
    });
  });
  await page.reload();
  await expect(page.getByRole("progressbar")).toHaveAttribute("aria-valuetext", "1,500 of 4,000 rows");
  const stop = page.getByRole("button", { name: "Stop import" });
  await stop.click();
  const dialog = page.getByRole("dialog", { name: `Stop the import of “apps-${stamp}.csv”?` });
  await expect(dialog).toContainText("1,500 of 4,000 rows are processed so far. Rows already imported stay imported. The import stops after the current batch of at most 500 rows.");
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await checkA11y(page, testInfo, "import-stop-dialog");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(stop).toBeFocused();
  await page.context().close();
});
