import AxeBuilder from "@axe-core/playwright";
import type { Browser, Page, TestInfo } from "@playwright/test";
import { apiGet, apiSend, at, classIdByName, expect, test } from "./support";

// Accessibility (WCAG 2.1 A and AA) of the main screens, checked with axe-core. A critical or serious
// violation fails the test; moderate and minor ones are listed in the output and attached to the report.
// No rule is turned off; one may only be turned off for a single screen, with a comment saying why.

const stamp = Date.now().toString(36);
const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
const FAILING = new Set(["critical", "serious"]);

/** Runs axe on the page as it is now; fails on critical and serious violations. */
async function checkA11y(page: Page, testInfo: TestInfo, name: string, options: { include?: string; disableRules?: string[] } = {}) {
  let builder = new AxeBuilder({ page }).withTags(WCAG_TAGS);
  if (options.include) builder = builder.include(options.include);
  if (options.disableRules?.length) builder = builder.disableRules(options.disableRules);
  const results = await builder.analyze();
  // "incomplete" are the checks axe could not decide (e.g. contrast over overlapping elements): for a manual look.
  const report = { url: page.url(), violations: results.violations, incomplete: results.incomplete };
  await testInfo.attach(`axe-${name}.json`, { body: JSON.stringify(report, null, 2), contentType: "application/json" });

  const describe = (v: (typeof results.violations)[number]) =>
    `[${v.impact}] ${v.id}: ${v.help} (${v.helpUrl})\n` + v.nodes.map((n) => `    ${n.target.join(" ")}: ${n.failureSummary?.replace(/\s+/g, " ")}`).join("\n");
  const reported = results.violations.filter((v) => !FAILING.has(v.impact ?? ""));
  if (reported.length) {
    testInfo.annotations.push({ type: "a11y (moderate/minor)", description: `${name}: ${reported.map((v) => v.id).join(", ")}` });
    console.log(`axe ${name}: ${reported.length} moderate/minor issue(s), not failing:\n${reported.map(describe).join("\n")}`);
  }
  const failing = results.violations.filter((v) => FAILING.has(v.impact ?? ""));
  expect(failing.map(describe), `critical/serious WCAG 2.1 AA violations on ${name}`).toEqual([]);
}

async function signedOutPage(browser: Browser): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  return context.newPage();
}

test("sign-in", async ({ browser }, testInfo) => {
  const page = await signedOutPage(browser);
  await page.goto("/login");
  await expect(page.getByRole("button", { name: "Sign in" })).toBeVisible();
  await checkA11y(page, testInfo, "sign-in");

  // The refused sign-in, with its error shown.
  await page.getByLabel("Username").fill(`nobody-${stamp}`);
  await page.getByLabel("Password").fill("not-the-password");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("alert")).toBeVisible({ timeout: 20_000 });
  await checkA11y(page, testInfo, "sign-in-refused");
  await page.context().close();
});

test("inventory list", async ({ page }, testInfo) => {
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.locator("table tbody tr").first()).toBeVisible();
  await checkA11y(page, testInfo, "inventory");
});

test("inventory Columns popover, light and dark", async ({ page, request }, testInfo) => {
  await page.goto(`/cis?classId=${await classIdByName(request, "Server")}`);
  await expect(page.locator("table tbody tr").first()).toBeVisible();
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await page.getByRole("button", { name: /^Columns/ }).click();
    await expect(page.getByRole("dialog", { name: "Columns" }).getByRole("group", { name: "Attributes of Server" })).toBeVisible();
    await checkA11y(page, testInfo, `columns-popover-${colorScheme}`);
    await page.keyboard.press("Escape");
  }
});

test("CI detail page (grid layout), its delete dialog and the edit form", async ({ page, request }, testInfo) => {
  // A demo-seed Server with its relationships, so the relationship table is checked too. Read-only: creating
  // a Server here would change which Server other specs pick as the first one by label (layout-edit).
  const serverId = await classIdByName(request, "Server");
  const ci = (await apiGet<{ data: { id: string; label: string }[] }>(request, `/configuration-items?classId=${serverId}&sort=label&limit=1`)).data[0];
  await page.goto(`/cis/${ci.id}`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(ci.label);
  await expect(page.getByRole("region", { name: "Relationships" }).getByRole("columnheader", { name: "Related CI" })).toBeVisible();
  await checkA11y(page, testInfo, "ci-detail");

  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await checkA11y(page, testInfo, "ci-delete-dialog");
  await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();

  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.getByRole("button", { name: "Save changes" })).toBeVisible();
  await checkA11y(page, testInfo, "ci-edit");
});

test("class and attribute editor", async ({ page, request }, testInfo) => {
  await page.goto(`/admin/classes/${await classIdByName(request, "Server")}`);
  await expect(page.getByRole("button", { name: "+ Add attribute" })).toBeVisible();
  await checkA11y(page, testInfo, "class-editor");

  await page.getByRole("button", { name: "+ Add attribute" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await checkA11y(page, testInfo, "attribute-dialog");
});

test("users and permission profiles", async ({ page }, testInfo) => {
  await page.goto("/admin/users");
  await expect(page.locator("table tbody tr").first()).toBeVisible();
  await checkA11y(page, testInfo, "users");

  await page.goto("/admin/users/new");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await checkA11y(page, testInfo, "user-new");
  // Required fields are announced as required; the red asterisk itself is hidden from screen readers.
  await expect(page.getByRole("textbox", { name: "Username", exact: true })).toHaveAttribute("aria-required", "true");

  await page.goto("/admin/profiles");
  await expect(page.locator("table tbody tr").first()).toBeVisible();
  await checkA11y(page, testInfo, "profiles");

  await page.locator("table tbody tr a").first().click();
  await expect(page.locator("#profile-name")).toBeVisible();
  await checkA11y(page, testInfo, "profile-edit");
});

test.describe("an operator's own account", () => {
  const USERNAME = `e2e-a11y-${stamp}`;
  const PASSWORD = `a11y-e2e-password-${stamp}`;
  let page: Page;

  test.beforeAll(async ({ request, browser }) => {
    await apiSend(request, "POST", "/admin/users", { username: USERNAME, displayName: `A11y operator ${stamp}`, password: PASSWORD, profileIds: [] });
    page = await signedOutPage(browser);
    await page.goto("/login");
    await page.getByLabel("Username").fill(USERNAME);
    await page.getByLabel("Password").fill(PASSWORD);
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page).toHaveURL(at("/"), { timeout: 20_000 });
  });
  test.afterAll(async () => page.context().close());

  test("account and password page", async ({}, testInfo) => {
    await page.goto("/account");
    await expect(page.getByRole("region", { name: "Password", exact: true })).toBeVisible();
    await checkA11y(page, testInfo, "account");
  });

  test("two-factor enrolment step (QR code and setup key)", async ({}, testInfo) => {
    await page.goto("/account");
    await page.locator("#mfa-currentPassword").fill(PASSWORD);
    await page.getByRole("button", { name: "Set up authenticator app" }).click();
    await expect(page.getByRole("img", { name: "QR code to add ShadouCMDB to your authenticator app" })).toBeVisible();
    await checkA11y(page, testInfo, "totp-enrolment");
  });
});
