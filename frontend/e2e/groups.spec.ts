import type { APIRequestContext } from "@playwright/test";
import { apiGet, apiSend, checkA11y, csrf, expect, test } from "./support";

// Administration › Groups (SHAA-927 §4.9, §5.8): list, create, edit with version, members, delete.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const GROUP = `E2E DBA team ${stamp}`;
const RENAMED = `E2E DB operations ${stamp}`;
const USERS = [`e2e-grp-a-${stamp}`, `e2e-grp-b-${stamp}`];
let groupId = "";

interface Group {
  id: string;
  name: string;
  description: string | null;
  memberCount: number;
  version: number;
  ownedServiceCount: number | null;
}

async function createUser(request: APIRequestContext, username: string): Promise<string> {
  const u = await apiSend<{ id: string }>(request, "POST", "/admin/users", {
    username,
    email: `${username}@example.test`,
    displayName: `Group member ${username}`,
    password: "group-member-password-1",
    isActive: true,
    profileIds: [],
  });
  return u.id;
}

test("Groups sits under Access, and an empty list says what groups are for", async ({ page }) => {
  await page.goto("/admin/users");
  const sub = page.getByRole("navigation", { name: "Administration" });
  await sub.getByRole("link", { name: "Groups", exact: true }).click();
  await expect(page).toHaveURL(/\/admin\/groups$/);
  await expect(page.getByRole("heading", { level: 1, name: "Groups" })).toBeVisible();

  // The database may already hold groups from other runs: show the empty state from an empty answer.
  await page.route("**/api/v1/admin/groups?*", (route) =>
    route.fulfill({ json: { data: [], page: { total: 0, limit: 50, offset: 0 } } }),
  );
  await page.reload();
  await expect(page.getByTestId("groups-empty")).toContainText(
    "No groups yet. Groups let you make a team the owner of a business service.",
  );
  await expect(page.getByTestId("groups-empty").getByRole("link", { name: "Create group" })).toBeVisible();
});

test("create a group; a duplicate name is refused next to the field", async ({ page, request }) => {
  await page.goto("/admin/groups");
  await page.getByRole("link", { name: "Create group" }).first().click();
  await expect(page).toHaveURL(/\/admin\/groups\/new$/);
  await page.getByRole("button", { name: "Create group" }).click();
  await expect(page.locator("#group-name")).toHaveAttribute("aria-invalid", "true");

  await page.getByLabel("Name").fill(GROUP);
  await page.getByLabel("Description").fill("Runs the PostgreSQL clusters");
  await page.getByRole("button", { name: "Create group" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Created group ${GROUP}.` })).toBeVisible();
  groupId = page.url().split("/").pop()!;
  const g = await apiGet<Group>(request, `/admin/groups/${groupId}`);
  expect(g).toMatchObject({ name: GROUP, description: "Runs the PostgreSQL clusters", memberCount: 0, ownedServiceCount: 0 });
  await expect(page.getByText("This group has no members yet.")).toBeVisible();

  // Same name in other case: 409 CONFLICT on `name`, shown at the field.
  await page.goto("/admin/groups/new");
  await page.getByLabel("Name").fill(GROUP.toUpperCase());
  await page.getByRole("button", { name: "Create group" }).click();
  await expect(page.locator("#group-name")).toHaveAttribute("aria-invalid", "true");
  await expect(page).toHaveURL(/\/admin\/groups\/new$/);
});

test("add and remove members through the user search", async ({ page, request }, testInfo) => {
  for (const u of USERS) await createUser(request, u);
  await page.goto(`/admin/groups/${groupId}`);

  const picker = page.getByRole("combobox", { name: "Add a member" });
  for (const u of USERS) {
    await picker.fill(u);
    await page.getByRole("option", { name: new RegExp(u) }).click();
    await expect(page.getByTestId("group-member-status")).toHaveText(`${u} was added to the group.`);
    await expect(page.getByRole("link", { name: u, exact: true })).toBeVisible();
  }
  await expect(page.getByText("2 members", { exact: true })).toBeVisible();
  expect((await apiGet<Group>(request, `/admin/groups/${groupId}`)).memberCount).toBe(2);

  // The member filter is server-side and lives in the URL.
  await page.getByLabel("Filter members").fill(USERS[1]);
  await expect(page).toHaveURL(new RegExp(`q=${USERS[1]}`));
  await expect(page.getByRole("link", { name: USERS[0], exact: true })).toBeHidden();
  await page.reload();
  await expect(page.getByLabel("Filter members")).toHaveValue(USERS[1]);
  await page.getByLabel("Filter members").fill("");
  await expect(page.getByRole("link", { name: USERS[0], exact: true })).toBeVisible();

  await checkA11y(page, testInfo, "group-edit");

  await page.getByRole("button", { name: `Remove ${USERS[0]} from ${GROUP}` }).click();
  await expect(page.getByTestId("group-member-status")).toContainText(`${USERS[0]} was removed from the group.`);
  await expect(page.getByRole("link", { name: USERS[0], exact: true })).toBeHidden();
  const members = await apiGet<{ data: { username: string }[] }>(request, `/admin/groups/${groupId}/members`);
  expect(members.data.map((m) => m.username)).toEqual([USERS[1]]);
});

test("a stale version is refused with 409 and the current version can be loaded", async ({ page, request }) => {
  await page.goto(`/admin/groups/${groupId}`);
  await expect(page.getByLabel("Name")).toHaveValue(GROUP);
  await page.getByLabel("Description").fill("Edited in the browser");
  // Someone else saves in between.
  const g = await apiGet<Group>(request, `/admin/groups/${groupId}`);
  await apiSend(request, "PATCH", `/admin/groups/${groupId}`, { version: g.version, description: "Edited elsewhere" });
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByTestId("group-conflict")).toContainText("Someone else changed this group while you were editing.");
  await page.getByRole("button", { name: "Load the current version" }).click();
  await expect(page.getByLabel("Description")).toHaveValue("Edited elsewhere");

  await page.getByLabel("Name").fill(RENAMED);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Saved ${RENAMED}.` })).toBeVisible();
  await expect(page.getByRole("heading", { level: 1, name: RENAMED })).toBeVisible();
});

test("the list searches and sorts on the server, in the URL", async ({ page }, testInfo) => {
  await page.goto("/admin/groups");
  await page.getByRole("searchbox", { name: "Search", exact: true }).fill(stamp);
  await expect(page).toHaveURL(new RegExp(`q=${stamp}`));
  await expect(page.getByRole("link", { name: RENAMED })).toBeVisible();
  await page.getByRole("button", { name: /^Members/ }).click();
  await expect(page).toHaveURL(/sort=memberCount/);
  await expect(page.getByRole("columnheader", { name: /Members/ })).toHaveAttribute("aria-sort", "ascending");
  await page.reload();
  await expect(page.getByRole("searchbox", { name: "Search", exact: true })).toHaveValue(stamp);
  await expect(page.getByRole("row").filter({ hasText: RENAMED }).getByRole("cell").nth(2)).toHaveText("1");
  await checkA11y(page, testInfo, "groups-list");
});

test("the delete dialog names the owned business services, or says the count is withheld", async ({ page }, testInfo) => {
  const detail = `**/api/v1/admin/groups/${groupId}`;
  await page.goto(`/admin/groups/${groupId}`);
  const real = await (await page.request.get(`/api/v1/admin/groups/${groupId}`)).json();

  await page.route(detail, (route) => route.fulfill({ json: { ...real, ownedServiceCount: 7 } }));
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete group" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete group ${RENAMED}?` });
  await expect(dialog.getByTestId("group-delete-services")).toHaveText(
    "It is owner of 7 business services; it is removed as owner from all of them.",
  );
  await expect(dialog).toContainText("Its 1 member loses the membership; the user accounts are not deleted.");
  await checkA11y(page, testInfo, "group-delete-dialog");
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await page.unroute(detail);
  await page.route(detail, (route) => route.fulfill({ json: { ...real, ownedServiceCount: null } }));
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete group" }).click();
  await expect(dialog.getByTestId("group-delete-services")).toHaveText(
    "It may be owner of business services you cannot view; it is removed as owner from all of them.",
  );
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await page.unroute(detail);

  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete group" }).click();
  await expect(dialog.getByTestId("group-delete-services")).toHaveText("It is not owner of any business service.");
  await dialog.getByRole("button", { name: "Delete group" }).click();
  await expect(page).toHaveURL(/\/admin\/groups$/);
  await expect(page.getByRole("status").filter({ hasText: `Deleted group ${RENAMED}.` })).toBeVisible();
  expect((await page.request.get(`/api/v1/admin/groups/${groupId}`)).status()).toBe(404);
});

test("deleting a user says how many business services lost them as owner", async ({ page, request }) => {
  const id = await createUser(request, `e2e-grp-del-${stamp}`);
  await page.goto(`/admin/users/${id}`);
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete user" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete user e2e-grp-del-${stamp}?` });
  await expect(dialog.getByTestId("user-delete-services")).toContainText("If they are owner of business services, they are removed as owner");
  await dialog.getByRole("button", { name: "Delete user" }).click();
  await expect(page).toHaveURL(/\/admin\/users$/);
  await expect(
    page.getByRole("status").getByText(`Deleted user e2e-grp-del-${stamp}. They were not owner of any business service.`, { exact: true }),
  ).toBeVisible();
  // Clean up the members created above.
  for (const u of USERS) {
    const list = await apiGet<{ data: { id: string }[] }>(request, `/admin/users?q=${u}`);
    for (const row of list.data)
      await request.delete(`/api/v1/admin/users/${row.id}`, { headers: { "X-CSRF-Token": await csrf(request) } });
  }
});
