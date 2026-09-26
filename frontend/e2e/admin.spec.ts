import type { Browser, Page } from "@playwright/test";
import { E2E_USER } from "./global-setup";
import { apiGet, ciIdByName, classIdByName, expect, snap, test } from "./support";

// One walk through Administration, in order: a profile, a user holding it, what that user can see, then account actions.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PROFILE = `E2E server editors ${stamp}`;
const USERNAME = `e2e-user-${stamp}`;
const DISPLAY = `E2E User ${stamp}`;
const PASSWORD = "first-password-123";
const NEW_PASSWORD = "second-password-456";
let profileId = "";
let userId = "";

interface Profile {
  id: string;
  name: string;
  globalPermissions: string[];
  classPermissions: { classId: string | null; view: boolean; create: boolean; edit: boolean; delete: boolean }[];
}

async function signInAs(browser: Browser, username: string, password: string): Promise<Page> {
  const context = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.goto("/login");
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  return page;
}

test("Administration has its own sub-navigation", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Administration", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/users$/);
  const sub = page.getByRole("navigation", { name: "Administration" });
  await expect(sub.getByRole("link")).toHaveText(["Users", "Permission profiles", "CI classes", "Relationship types", "Lookups", "Templates", "Audit log"]);
  await expect(page.getByRole("heading", { level: 1, name: "Users" })).toBeVisible();
  await expect(page.getByRole("cell", { name: E2E_USER.username, exact: true })).toBeVisible();
});

test("create a permission profile from the matrix", async ({ page, request }) => {
  await page.goto("/admin/profiles");
  await expect(page.getByRole("link", { name: "Administrator", exact: true })).toBeVisible();
  await page.getByRole("link", { name: "+ New profile" }).click();
  await page.locator("#profile-name").fill(PROFILE);
  await page.locator("#profile-description").fill("Sees everything, edits servers");
  await page.getByLabel("View the audit log").check();
  await page.getByLabel("view on all classes").check();
  // The wildcard's view shows on every class row, checked and locked.
  await expect(page.getByLabel("view on Database", { exact: true })).toBeChecked();
  await expect(page.getByLabel("view on Database", { exact: true })).toBeDisabled();
  await page.getByLabel("edit on Server", { exact: true }).check();
  await expect(page.getByLabel("edit on Server", { exact: true })).toBeChecked();
  await snap(page, "23-profile-matrix");
  await page.getByRole("button", { name: "Create profile" }).click();
  await expect(page.getByRole("status")).toContainText(`Created profile ${PROFILE}.`);
  profileId = page.url().split("/").pop()!;

  const saved = await apiGet<Profile>(request, `/admin/profiles/${profileId}`);
  const serverId = await classIdByName(request, "Server");
  expect(saved.globalPermissions).toEqual(["audit.view"]);
  expect(saved.classPermissions).toEqual(
    expect.arrayContaining([
      { classId: null, view: true, create: false, edit: false, delete: false },
      { classId: serverId, view: true, create: false, edit: true, delete: false },
    ]),
  );
  expect(saved.classPermissions).toHaveLength(2);
});

test("the built-in Administrator profile is read-only; clone and delete a profile", async ({ page }) => {
  await page.goto("/admin/profiles");
  await page.getByRole("link", { name: "Administrator", exact: true }).click();
  await expect(page.getByRole("note")).toContainText("cannot be changed or deleted");
  await expect(page.getByLabel("delete on all classes")).toBeChecked();
  await expect(page.getByLabel("delete on all classes")).toBeDisabled();
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0);

  await page.goto(`/admin/profiles/${profileId}`);
  await page.getByRole("button", { name: "Clone" }).click();
  const dialog = page.getByRole("dialog", { name: /^Clone/ });
  await expect(dialog.getByLabel(/Name of the copy/)).toHaveValue(`Copy of ${PROFILE}`);
  await dialog.getByRole("button", { name: "Clone profile" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(`Copy of ${PROFILE}`);
  await expect(page.getByLabel("edit on Server", { exact: true })).toBeChecked();

  await page.getByRole("button", { name: "Delete" }).click();
  const confirm = page.getByRole("dialog", { name: `Delete profile “Copy of ${PROFILE}”?` });
  await expect(confirm).toContainText("No user holds this profile");
  await confirm.getByRole("button", { name: "Delete profile" }).click();
  await expect(page).toHaveURL(/\/admin\/profiles$/);
  await expect(page.getByRole("link", { name: `Copy of ${PROFILE}`, exact: true })).toHaveCount(0);
});

test("create a user holding the profile", async ({ page }) => {
  await page.goto("/admin/users/new");
  await page.getByRole("button", { name: "Create user" }).click();
  await expect(page.locator("#user-username-err")).toHaveText("Required");
  await page.locator("#user-username").fill(USERNAME);
  await page.locator("#user-displayName").fill(DISPLAY);
  await page.locator("#user-password").fill(PASSWORD);
  await page.locator("#user-confirm").fill(PASSWORD);
  await page.getByLabel(PROFILE).check();
  await page.getByRole("button", { name: "Create user" }).click();
  await expect(page.getByRole("status").first()).toContainText(`Created user ${USERNAME}.`);
  userId = page.url().split("/").pop()!;
  await snap(page, "24-user-created");

  // The API's own validation is shown next to the field.
  await page.goto("/admin/users/new");
  await page.locator("#user-username").fill("has spaces");
  await page.locator("#user-displayName").fill("x");
  await page.locator("#user-password").fill(PASSWORD);
  await page.locator("#user-confirm").fill(PASSWORD);
  await page.getByRole("button", { name: "Create user" }).click();
  await expect(page.locator("#user-username")).toHaveAttribute("aria-invalid", "true");
  await expect(page.locator("#user-username-err")).not.toBeEmpty();

  await page.goto(`/admin/users?profileId=${profileId}`);
  await expect(page.getByRole("link", { name: USERNAME, exact: true })).toBeVisible();
  await expect(page.getByRole("row")).toHaveCount(2); // header + the one holder
});

test("the user sees only the actions their profile allows", async ({ browser, request }) => {
  const page = await signInAs(browser, USERNAME, PASSWORD);
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await expect(page.getByRole("banner").getByText(DISPLAY)).toBeVisible();
  await expect(page.getByRole("link", { name: "+ New CI" })).toHaveCount(0);

  // Server: may edit, may not delete.
  await page.goto(`/cis/${await ciIdByName(request, "fra1-esx-01")}`);
  await expect(page.getByRole("heading", { level: 1, name: "fra1-esx-01" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Edit", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0);
  await expect(page.getByRole("tab", { name: "History" })).toBeVisible(); // audit.view
  await snap(page, "25-limited-user-ci");

  // Database: view only.
  await page.goto(`/cis/${await ciIdByName(request, "crm-db")}`);
  await expect(page.getByRole("heading", { level: 1, name: "crm-db" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Edit", exact: true })).toHaveCount(0);
  await page.goto(`/cis/${await ciIdByName(request, "crm-db")}/edit`);
  await expect(page.getByRole("heading", { name: "Permission denied" })).toBeVisible();

  // Administration: only the audit log.
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Administration", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/audit$/);
  await expect(page.getByRole("navigation", { name: "Administration" }).getByRole("link")).toHaveText(["Audit log"]);
  await page.goto("/admin/users");
  await expect(page.getByRole("heading", { name: "Permission denied" })).toBeVisible();
  await page.context().close();
});

test("disable, enable and reset the password; the audit log names who did it", async ({ page, browser }) => {
  await page.goto(`/admin/users/${userId}`);
  await page.getByRole("button", { name: "Disable account" }).click();
  const confirm = page.getByRole("dialog", { name: `Disable ${USERNAME}?` });
  await expect(confirm).toContainText("signed out everywhere");
  await confirm.getByRole("button", { name: "Disable account" }).click();
  await expect(page.locator(".page-header .badge").first()).toHaveText("Disabled");

  const blocked = await signInAs(browser, USERNAME, PASSWORD);
  await expect(blocked.getByRole("alert")).toContainText("Wrong username or password, or the account is disabled");
  await blocked.context().close();

  await page.getByRole("button", { name: "Enable account" }).click();
  await page.getByRole("dialog", { name: `Enable ${USERNAME}?` }).getByRole("button", { name: "Enable account" }).click();
  await expect(page.locator(".page-header .badge").first()).toHaveText("Active");

  await page.locator("#reset-password").fill(NEW_PASSWORD);
  await page.locator("#reset-confirm").fill(NEW_PASSWORD);
  await page.getByRole("button", { name: "Set new password" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Password changed" })).toBeVisible();
  const again = await signInAs(browser, USERNAME, NEW_PASSWORD);
  await expect(again.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await again.context().close();

  await page.getByRole("link", { name: "Changes by this user" }).click();
  await expect(page).toHaveURL(new RegExp(`/admin/audit\\?actorId=${userId}$`));
  await page.goto("/admin/audit?entityType=users");
  const row = page.getByRole("row").filter({ hasText: USERNAME }).filter({ hasText: "create" });
  await expect(row).toHaveCount(1);
  await expect(row.getByRole("link", { name: E2E_USER.username, exact: true })).toBeVisible();
  await snap(page, "26-audit-log");
});
