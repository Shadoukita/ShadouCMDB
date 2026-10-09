import type { Browser, Page } from "@playwright/test";
import { apiSend, checkA11y, ciIdByName, classIdByName, expect, snap, test } from "./support";

// A screen the operator may not open, in the reference-mockup look (design document §2.7, step 10-7): the page head
// band with a lock tile, "Permission denied", an "Error 403" badge and what is missing (a global permission by name
// with its key in mono, or a class right in words), then a panel with the explanation and the way on. One shared
// state for Administration, Bulk import, Business services and the CI pages, instead of a bare empty state each.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const USERNAME = `e2e-denied-${stamp}`;
const PASSWORD = "denied-password-123";
let crmId = "";

test.beforeAll(async ({ request }) => {
  const applicationId = await classIdByName(request, "Application");
  crmId = await ciIdByName(request, "CRM");
  // The audit log only (so Administration has one section), and Applications read-only. No Bulk import, no services.
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E denied ${stamp}`,
    globalPermissions: ["audit.view"],
    classPermissions: [{ classId: applicationId, view: true, create: false, edit: false, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", {
    username: USERNAME,
    email: `${USERNAME}@example.test`,
    displayName: `E2E Denied ${stamp}`,
    password: PASSWORD,
    profileIds: [profile.id],
  });
});

async function signIn(browser: Browser, opts: { theme?: "dark"; density?: "comfortable"; locale?: "de"; width?: number } = {}): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] }, viewport: { width: opts.width ?? 1440, height: 900 } });
  const page = await context.newPage();
  await page.addInitScript((o) => {
    if (o.theme) localStorage.setItem("shadoucmdb.theme", o.theme);
    if (o.density) localStorage.setItem("shadoucmdb.density", o.density);
    if (o.locale) (window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = o.locale;
  }, opts);
  await page.goto("/login");
  await page.locator("#login-username").fill(USERNAME);
  await page.locator("#login-password").fill(PASSWORD);
  await page.locator("form button[type=submit]").click();
  await expect(page.locator(".record-head, .page-header").first()).toBeVisible();
  await expect(page).not.toHaveURL(/\/login/);
  return page;
}

/** The shared state: the head band and the panel below it. */
function denied(page: Page) {
  const head = page.getByTestId("permission-denied");
  return { head, panel: page.locator("main section.panel").last() };
}

test("a global permission: the head band names it with its key, the panel explains", async ({ browser }, testInfo) => {
  const page = await signIn(browser);
  await page.goto("/admin/users");
  const { head, panel } = denied(page);
  await expect(head.getByRole("heading", { level: 1, name: "Permission denied" })).toBeVisible();
  await expect(head.locator(".class-tile")).toBeVisible();
  // The title names a state, not a record: the UI font.
  expect(await head.locator("h1").evaluate((el) => getComputedStyle(el).fontFamily)).toMatch(/^"IBM Plex Sans"/);
  await expect(head.getByTestId("record-meta").locator(".badge")).toHaveText("Error 403");
  const needs = head.getByTestId("denied-needs");
  await expect(needs).toContainText("Needs");
  await expect(needs).toContainText("Manage users");
  const key = needs.locator("code");
  await expect(key).toHaveText("users.manage");
  expect(await key.evaluate((el) => getComputedStyle(el).fontFamily)).toContain("Plex Mono");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Administration");

  await expect(panel.getByRole("heading", { level: 2, name: "This Administration screen is not open to you" })).toBeVisible();
  await expect(panel.locator(".state-icon")).toBeVisible();
  await expect(panel.getByRole("link", { name: "Go to the dashboard" })).toHaveClass(/btn-primary/);
  // The section the user does hold stays reachable beside the state.
  await expect(page.getByRole("navigation", { name: "Administration" }).getByRole("link", { name: "Audit log" })).toBeVisible();
  await expect(page.locator("main [style]")).toHaveCount(0);
  await expect(page.getByRole("heading", { level: 1 })).toHaveCount(1);

  await snap(page, "denied-admin-en-light");
  await checkA11y(page, testInfo, "denied-admin-light");
  await panel.getByRole("link", { name: "Go to the dashboard" }).click();
  await expect(page).toHaveURL(/\/$/);
  await page.context().close();
});

test("Bulk import names its permission and leads back to the inventory", async ({ browser }) => {
  const page = await signIn(browser);
  await page.goto("/imports");
  const { head, panel } = denied(page);
  await expect(head.getByRole("heading", { level: 1, name: "Permission denied" })).toBeVisible();
  await expect(head.getByTestId("denied-needs")).toContainText("Bulk import");
  await expect(head.getByTestId("denied-needs").locator("code")).toHaveText("cis.import");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Imports");
  await expect(panel.getByRole("heading", { level: 2, name: "Bulk import is not open to you" })).toBeVisible();
  await expect(panel.getByText("You need the Bulk import permission.")).toBeVisible();
  await panel.getByRole("link", { name: "Back to inventory" }).click();
  await expect(page).toHaveURL(/\/cis$/);
  await page.context().close();
});

test("a class right: the edit page names it in words and leads back to the record", async ({ browser }) => {
  const page = await signIn(browser);
  await page.goto(`/cis/${crmId}/edit`);
  const { head, panel } = denied(page);
  await expect(head.getByRole("heading", { level: 1, name: "Permission denied" })).toBeVisible();
  await expect(head.getByTestId("denied-needs")).toHaveText("Needs the Edit right on Application");
  await expect(head.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: "CRM" })).toBeVisible();
  await expect(panel.getByRole("heading", { level: 2, name: "This configuration item is not open to you" })).toBeVisible();
  await expect(panel).toContainText("None of your permission profiles allows editing Application configuration items.");
  await panel.getByRole("link", { name: "Back to the record" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${crmId}$`));
  await expect(page.getByRole("heading", { level: 1, name: "CRM" })).toBeVisible();
  await page.context().close();
});

test("Business services: the class right in words, the page header gives way", async ({ browser }) => {
  const page = await signIn(browser);
  await page.goto("/services");
  const { head, panel } = denied(page);
  await expect(head.getByRole("heading", { level: 1, name: "Permission denied" })).toBeVisible();
  await expect(head.getByTestId("denied-needs")).toHaveText("Needs the View right on the business service class");
  await expect(page.getByRole("heading", { level: 1 })).toHaveCount(1);
  await expect(panel.getByText("You do not have permission to view business services.")).toBeVisible();
  await page.context().close();
});

test("the dark theme", async ({ browser }, testInfo) => {
  const page = await signIn(browser, { theme: "dark" });
  await page.goto(`/cis/${crmId}/edit`);
  await expect(page.getByRole("heading", { level: 1, name: "Permission denied" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await checkA11y(page, testInfo, "denied-ci-dark");
  await page.goto("/admin/users");
  await expect(page.getByTestId("denied-needs").locator("code")).toHaveText("users.manage");
  await checkA11y(page, testInfo, "denied-admin-dark");
  await page.context().close();
});

test("the texts come from the German catalog and fit at 900 px", async ({ browser }) => {
  const page = await signIn(browser, { locale: "de", width: 900 });
  await page.goto("/admin/users");
  const { head, panel } = denied(page);
  await expect(head.getByRole("heading", { level: 1, name: "Keine Berechtigung" })).toBeVisible();
  await expect(head.getByTestId("record-meta")).toContainText("Fehler 403");
  await expect(head.getByTestId("denied-needs")).toContainText("Erfordert");
  await expect(panel.getByRole("heading", { level: 2, name: "Diese Seite der Administration ist für Sie nicht freigegeben" })).toBeVisible();
  expect(await page.locator("main").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.goto(`/cis/${crmId}/edit`);
  await expect(page.getByTestId("denied-needs")).toHaveText("Erfordert das Recht Bearbeiten auf Application");
  await expect(page.getByRole("link", { name: "Zurück zum Datensatz" })).toBeVisible();
  expect(await page.locator("main").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.context().close();
});

// Screenshots for the review (E2E_SCREENSHOT_DIR): two screens × both themes, both densities, en and de, 1440 and 900 px.
for (const theme of ["light", "dark"] as const)
  for (const density of ["compact", "comfortable"] as const)
    for (const locale of ["en", "de"] as const)
      for (const width of [1440, 900])
        test(`permission denied screenshots: ${theme}, ${density}, ${locale}, ${width}`, async ({ browser }) => {
          test.skip(!process.env.E2E_SCREENSHOT_DIR, "screenshots only");
          const page = await signIn(browser, {
            theme: theme === "dark" ? "dark" : undefined,
            density: density === "comfortable" ? "comfortable" : undefined,
            locale: locale === "de" ? "de" : undefined,
            width,
          });
          for (const [name, path] of [
            ["admin", "/admin/users"],
            ["ci-edit", `/cis/${crmId}/edit`],
          ]) {
            await page.goto(path);
            await expect(page.getByTestId("denied-needs")).toBeVisible();
            await snap(page, `denied-${name}-${locale}-${theme}-${density}-${width}`);
          }
          await page.context().close();
        });
