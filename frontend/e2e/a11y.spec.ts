import type { Browser, Page } from "@playwright/test";
import { apiGet, apiSend, at, checkA11y, classIdByName, expect, test } from "./support";

// Accessibility (WCAG 2.1 A and AA) of the main screens, checked with axe-core. A critical or serious
// violation fails the test; moderate and minor ones are listed in the output and attached to the report.
// No rule is turned off; one may only be turned off for a single screen, with a comment saying why.

const stamp = Date.now().toString(36);

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

test("CI detail page (built-in layout), its delete dialog and the edit form", async ({ page, request }, testInfo) => {
  // A demo-seed Server with its relationships, so the relationship table is checked too. Read-only: creating
  // a Server here would change which Server other specs pick as the first one by label (layout-edit).
  const serverId = await classIdByName(request, "Server");
  const ci = (await apiGet<{ data: { id: string; label: string }[] }>(request, `/configuration-items?classId=${serverId}&sort=label&limit=1`)).data[0];
  await page.goto(`/cis/${ci.id}`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(ci.label);
  await expect(page.getByRole("region", { name: "Relationships" }).getByRole("searchbox", { name: "Filter relationships" })).toBeVisible();
  // The record header (design §0 step 12d): the tab counts and the history have loaded before the scan.
  await expect(page.getByRole("tab", { name: /^Relationship map \d+$/ })).toBeVisible();
  await expect(page.locator("ol.event-timeline > li").first()).toBeVisible();
  await checkA11y(page, testInfo, "ci-detail");

  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await checkA11y(page, testInfo, "ci-delete-dialog");
  await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();

  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.getByRole("button", { name: "Save changes" })).toBeVisible();
  await checkA11y(page, testInfo, "ci-edit");
});

test("CI topology panel (design §2.7, audit R6)", async ({ page, request }, testInfo) => {
  const serverId = await classIdByName(request, "Server");
  const ci = (await apiGet<{ data: { id: string; label: string }[] }>(request, `/configuration-items?classId=${serverId}&sort=label&limit=1`)).data[0];
  await page.goto(`/cis/${ci.id}`);
  await page.getByRole("tab", { name: "Relationship map" }).click();
  await expect(page.getByRole("img", { name: new RegExp(`^${ci.label} and \\d+ related CIs? within 1 hop`) })).toBeVisible();
  await expect(page.getByRole("tree", { name: "Relationship map" }).getByRole("treeitem").first()).toBeVisible();
  await checkA11y(page, testInfo, "ci-topology");
});

test("Customization › Layouts: the class picker and Edit CI", async ({ page }, testInfo) => {
  await page.goto("/admin/customization/layouts?class=server");
  await expect(page.getByTestId("layout-edit-ci")).toHaveText(/^Edit CI: .+/);
  await checkA11y(page, testInfo, "customization-layouts");
});

test("class and attribute editor", async ({ page, request }, testInfo) => {
  await page.goto(`/admin/classes/${await classIdByName(request, "Server")}`);
  await expect(page.getByRole("button", { name: "Add attribute", exact: true })).toBeVisible();
  await checkA11y(page, testInfo, "class-editor");

  await page.getByRole("button", { name: "Add attribute", exact: true }).click();
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

test("dismissing a toast by keyboard keeps focus in the page (WCAG 2.4.3)", async ({ page }) => {
  // Create a group and delete it again: two toasts in one page session ("Created", then "Deleted").
  // A second page.goto would reload the app and drop the first toast with it.
  const name = `e2e-toast-${stamp}`;
  await page.goto("/admin/groups/new");
  await page.getByLabel("Name").fill(name);
  await page.getByRole("button", { name: "Create group" }).click();
  await expect(page).toHaveURL(/\/admin\/groups\/[^/]+$/);
  await expect(page).not.toHaveURL(/\/new$/);
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete group" }).click();
  await page.getByRole("dialog", { name: `Delete group ${name}?` }).getByRole("button", { name: "Delete group" }).click();
  await expect(page).toHaveURL(/\/admin\/groups$/);

  // Focus enters the stack from the search field, as it would by Tab; focus in the stack stops the clocks.
  const dismiss = page.getByRole("button", { name: "Dismiss notification" });
  await expect(dismiss).toHaveCount(2);
  const search = page.locator("#g-q");
  await search.focus();
  await dismiss.first().focus();

  // The first toast goes; focus moves to the close button of the one that is left.
  await page.keyboard.press("Enter");
  await expect(dismiss).toHaveCount(1);
  await expect(dismiss).toBeFocused();

  // The last toast goes; focus returns to where it was before the stack.
  await page.keyboard.press("Enter");
  await expect(dismiss).toHaveCount(0);
  await expect(search).toBeFocused();
});

test.describe("an operator's own account", () => {
  const USERNAME = `e2e-a11y-${stamp}`;
  const PASSWORD = `a11y-e2e-password-${stamp}`;
  let page: Page;

  test.beforeAll(async ({ request, browser }) => {
    await apiSend(request, "POST", "/admin/users", { username: USERNAME, email: `${USERNAME}@example.test`, displayName: `A11y operator ${stamp}`, password: PASSWORD, profileIds: [] });
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
