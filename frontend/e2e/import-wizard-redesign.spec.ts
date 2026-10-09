import type { APIRequestContext } from "@playwright/test";
import { apiSend, checkA11y, chooseTheme, csrf, expect, test } from "./support";

// The import wizard in the reference-mockup look (design document §0, step 9c-2): the CI page head band (breadcrumb,
// the file name in mono, status / class / rows chips, Started …), the stepper with connectors and done states (M1),
// and one Stop pattern (M2): a secondary Stop import in the head while the job runs, Delete import in the head's
// menu once it has stopped, both with the Imports list's confirmation. Import is on only for this spec.
test.describe.configure({ mode: "serial" });

const FILE = `wizard-${Date.now().toString(36)}.csv`;
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

test("import wizard: head band, stepper with done states, Delete in the head menu", async ({ page }, testInfo) => {
  await page.goto(`/imports/${jobId}`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Imports");
  const h1 = head.getByRole("heading", { level: 1, name: FILE });
  await expect(h1).toHaveCSS("font-family", /IBM Plex Mono/);
  const meta = head.getByTestId("record-meta");
  await expect(meta.locator(".badge").first()).toHaveText(/Ready to map/, { timeout: 15_000 });
  await expect(meta).toContainText("1 row");
  await expect(meta.locator("time")).toContainText(/^Started /);

  // The stepper: Upload is current, Map columns can be opened, Check and Import are still ahead.
  const steps = page.getByRole("navigation", { name: "Import steps" });
  await expect(steps.locator("li[aria-current=step]")).toHaveText(/Upload/);
  await expect(steps.getByRole("link", { name: "Map columns" })).toBeVisible();
  await expect(steps.locator("li.future")).toHaveCount(2);
  await expect(steps.locator("li.current .num")).toHaveCSS("border-radius", "50%");

  // Delete sits in the head's menu once the job has stopped, and asks first.
  await head.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete import" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete the import of “${FILE}”?` });
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await checkA11y(page, testInfo, "import-wizard-file-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "import-wizard-file-dark");
  await chooseTheme(page, "");

  // On to the mapping: Upload is done, with a check and "(done)" for assistive tech.
  await page.getByRole("button", { name: "Next: Map columns" }).click();
  await expect(steps.locator("li[aria-current=step]")).toHaveText(/Map columns/);
  const done = steps.locator("li.done");
  await expect(done).toHaveCount(1);
  await expect(done.locator(".num svg")).toHaveCount(1);
  await expect(done).toContainText("Upload (done)");
  await checkA11y(page, testInfo, "import-wizard-mapping-light");
});

test("import wizard: one Stop pattern, a secondary button in the head that asks first", async ({ page }, testInfo) => {
  // Replay the job as if its check were running: Stop import replaces the head menu.
  await page.route(`**/api/v1/imports/${jobId}`, async (route) => {
    const res = await route.fetch();
    const job = await res.json();
    await route.fulfill({
      response: res,
      json: { ...job, status: "validating", phase: "validate", progress: { ...job.progress, done: 1, total: 1, startedAt: new Date().toISOString() } },
    });
  });
  await page.goto(`/imports/${jobId}`);
  const head = page.locator(".record-head");
  const stop = head.getByRole("button", { name: "Stop import" });
  await expect(stop).toBeVisible();
  await expect(stop).not.toHaveClass(/\bbtn-danger\b/);
  await expect(head.getByRole("button", { name: "More actions" })).toHaveCount(0);
  // The step itself has no second Stop or Cancel button.
  await expect(page.locator("main section.panel").getByRole("button", { name: /^(Stop import|Cancel)$/ })).toHaveCount(0);
  await stop.click();
  const dialog = page.getByRole("dialog", { name: `Stop the import of “${FILE}”?` });
  await expect(dialog).toContainText("Nothing has been imported yet");
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await checkA11y(page, testInfo, "import-wizard-stop-dialog");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(stop).toBeFocused();
  await page.unrouteAll({ behavior: "ignoreErrors" });
});

test("import wizard: the page texts come from the German catalog", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto(`/imports/${jobId}?step=1`);
  const head = page.locator(".record-head");
  await expect(head.getByRole("navigation", { name: "Navigationspfad" })).toContainText("Importe");
  await expect(head.getByTestId("record-meta")).toContainText("1 Zeile");
  await expect(head.getByTestId("record-meta").locator("time")).toContainText(/^Gestartet /);
  const steps = page.getByRole("navigation", { name: "Importschritte" });
  await expect(steps.locator("li[aria-current=step]")).toHaveText(/Hochladen/);
  await expect(steps.getByRole("link", { name: "Spalten zuordnen" })).toBeVisible();
  await expect(page.getByRole("heading", { level: 2, name: "Hochladen" })).toBeVisible();
  await expect(head.getByRole("button", { name: "Weitere Aktionen" })).toBeVisible();
});
