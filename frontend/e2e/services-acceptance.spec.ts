import type { APIRequestContext, Browser, Page } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { apiGet, apiSend, checkA11y, classIdByName, csrf, expect, lookupValueId, test } from "./support";

// Business services U5 (SHAA-936): the acceptance walk of spec SHAA-927 §7.4 and the §7.5 axe sweep.
// Every axe scan here is strict (zero violations of any impact, WCAG 2.1 A/AA).
//
// §7.4 scenario → test. Scenarios marked "also" have more detail in the U2/U3/U4 specs:
//    1 grant → nav entry, no grant → none and the permission page          here
//    2 create, land in the Owners editor, user + group, list shows both     here; also business-services.spec.ts
//    3 picker across two pages, "Already a member", add 3, count + live     here
//    4 a service that includes this one → per-item cycle error, none added  here; also service-members.spec.ts
//    5 remove selected (confirm names the number), row menu remove          here
//    6 list and Members filters: reload, Back and Forward                   here; also both specs above
//    7 "Part of business services" with the "via" chain that navigates      service-members.spec.ts
//    8 the member's pinned "Affected business services", service → Upstream service-members.spec.ts, business-services.spec.ts
//    9 restricted profile (§7.2): count 2, note, absent from list/CSV/class here
//   10 delete a nested service: dialog counts, gone from the parent         here
//   11 groups admin: create, add member, delete while owner, with count     here; also groups.spec.ts
//   12 409 owners conflict from two tabs                                    here
//   13 German catalog through the test-only locale override                 here; key parity in unit/i18n.test.ts
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const PASSWORD = "acceptance-password-123";
const OPERATOR = `e2e-acc-op-${stamp}`;
const RESTRICTED = `e2e-acc-r-${stamp}`;
const TECH = `Tess Tech ${stamp}`;
const TECH2 = `Theo Tech ${stamp}`;
const GROUP = `Acc Business team ${stamp}`;
const GROUP11 = `Acc Owners group ${stamp}`;
const SHOP = `Acc Shop ${stamp}`;
const OUTER = `Acc Outer ${stamp}`;
const R_SVC = `Acc Restricted ${stamp}`;
const INNER10 = `Acc Inner ${stamp}`;
const PARENT_A = `Acc Parent A ${stamp}`;
const PARENT_B = `Acc Parent B ${stamp}`;
/** 55 servers: more than one picker page (50). Labels sort as pg<stamp>-001 … -055. */
const PG = `pg${stamp}`;
const pg = (n: number) => `${PG}-${String(n).padStart(3, "0")}`;
const WEB1 = `svc-acc-web-01-${stamp}`;
const WEB2 = `svc-acc-web-02-${stamp}`;
const DB1 = `svc-acc-db-01-${stamp}`;

const ids: Record<string, string> = {};
let classId = "";
let className = "";
let operatorProfile = "";

interface Owners {
  version: number;
  owners: { technical: { id: string }[]; business: { id: string }[] };
}
const service = (request: APIRequestContext, id: string) => apiGet<Owners>(request, `/business-services/${id}`);
const members = (request: APIRequestContext, id: string) =>
  apiGet<{ data: { ci: { id: string } }[]; page: { total: number } }>(request, `/business-services/${id}/members?limit=200`);

const user = async (request: APIRequestContext, username: string, displayName: string, profileIds: string[] = []) =>
  (await apiSend<{ id: string }>(request, "POST", "/admin/users", { username, email: `${username}@example.test`, displayName, password: PASSWORD, profileIds })).id;

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

/** Picks a user or group in the owner combobox labelled `label` by typing its name and pressing Enter on the match. */
async function pickOwner(page: Page, label: string, name: string) {
  const box = page.getByRole("combobox", { name: label, exact: true });
  await box.fill(name);
  await expect(page.getByRole("option").filter({ hasText: name }).first()).toBeVisible();
  await expect(box).toHaveAttribute("aria-activedescendant", /.+/);
  await box.press("Enter");
}

const memberLinks = (page: Page) => page.locator(".service-members tbody tr td:nth-child(2) a");
const picker = (page: Page, name: string) => page.getByRole("dialog", { name: `Add members to ${name}` });

test.beforeAll(async ({ request }) => {
  test.setTimeout(120_000);
  classId = (await apiGet<{ classId: string }>(request, "/settings/business-services")).classId;
  className = (await apiGet<{ name: string }>(request, `/ci-classes/${classId}`)).name;
  const server = await classIdByName(request, "Server");
  const database = await classIdByName(request, "Database");
  const status = await lookupValueId(request, "status", "in_service");
  const ci = async (cls: string, name: string, attributes: Record<string, unknown> = {}) =>
    (await apiSend<{ id: string }>(request, "POST", "/configuration-items", { classId: cls, attributes: { name, status, ...attributes } })).id;

  ids.tech = await user(request, `e2e-acc-tess-${stamp}`, TECH);
  ids.tech2 = await user(request, `e2e-acc-theo-${stamp}`, TECH2);
  ids.group = (await apiSend<{ id: string }>(request, "POST", "/admin/groups", { name: GROUP, description: "e2e acceptance" })).id;

  // Scenario 1: a profile without the service class; the admin grants it in the UI.
  operatorProfile = (await apiSend<{ id: string }>(request, "POST", "/admin/profiles", { name: `E2E acc operators ${stamp}`, globalPermissions: [], classPermissions: [] })).id;
  await user(request, OPERATOR, OPERATOR, [operatorProfile]);

  // Scenario 9 (§7.2 profile R): view/edit on the service class, view on Server, nothing on Database.
  const r = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E acc restricted ${stamp}`,
    globalPermissions: [],
    classPermissions: [
      { classId, view: true, create: false, edit: true, delete: false },
      { classId: server, view: true, create: false, edit: false, delete: false },
    ],
  });
  await user(request, RESTRICTED, RESTRICTED, [r.id]);
  ids.rsvc = await ci(classId, R_SVC);
  ids.web1 = await ci(server, WEB1);
  ids.web2 = await ci(server, WEB2);
  ids.db1 = await ci(database, DB1, { engine: "postgresql" });
  await apiSend(request, "POST", `/business-services/${ids.rsvc}/members`, { memberIds: [ids.web1, ids.web2, ids.db1] });

  for (let n = 1; n <= 55; n++) ids[pg(n)] = await ci(server, pg(n));
  ids.outer = await ci(classId, OUTER);
  ids.inner10 = await ci(classId, INNER10);
  ids.parentA = await ci(classId, PARENT_A);
  ids.parentB = await ci(classId, PARENT_B);
  await apiSend(request, "POST", `/business-services/${ids.inner10}/members`, { memberIds: [ids[pg(54)], ids[pg(55)]] });
  for (const p of [ids.parentA, ids.parentB]) await apiSend(request, "POST", `/business-services/${p}/members`, { memberIds: [ids.inner10] });
});

test("1: a granted profile shows the nav entry; without the grant there is none and a direct URL is refused", async ({ page, browser }) => {
  const operator = await signInUi(browser, OPERATOR);
  try {
    const nav = operator.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Business services", exact: true });
    await expect(nav).toHaveCount(0);
    await operator.goto("/services");
    await expect(operator.getByText("You do not have permission to view business services.")).toBeVisible();

    // The administrator grants view on the service class in the permission matrix.
    await page.goto(`/admin/profiles/${operatorProfile}`);
    await page.getByLabel(`view on ${className}`, { exact: true }).check();
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page.getByRole("status").filter({ hasText: /^Saved E2E acc operators/ })).toBeVisible();

    await operator.goto("/");
    await nav.click();
    await expect(operator).toHaveURL(/\/services$/);
    await expect(operator.getByRole("heading", { level: 1, name: "Business services" })).toBeVisible();
    await expect(operator.locator("table.data")).toBeVisible();
    // View only: no create action.
    await expect(operator.getByRole("link", { name: "Create business service" })).toHaveCount(0);
  } finally {
    await operator.context().close();
  }
});

test("2: create a service, land in the Owners editor, save a user and a group; the list shows both", async ({ page }, testInfo) => {
  await page.goto("/services");
  await page.getByRole("link", { name: "Create business service" }).first().click();
  await page.locator("#attr-name").fill(SHOP);
  await page.locator("#attr-status").selectOption({ label: "In service" });
  await page.getByRole("button", { name: /^Create / }).click();

  await expect(page).toHaveURL(/\/services\/[0-9a-f-]{36}$/);
  ids.shop = page.url().split("/").pop()!;
  await expect(page.getByRole("combobox", { name: "Add to Technical owners" })).toBeFocused();
  await pickOwner(page, "Add to Technical owners", TECH);
  await pickOwner(page, "Add to Business owners", GROUP);
  await expect(page.locator(".token-list").last()).toContainText(GROUP);
  await checkA11y(page, testInfo, "owners editor open", { strict: true });
  await page.getByRole("button", { name: "Save owners" }).click();

  const card = page.locator(".owners-card");
  await expect(card.getByRole("list", { name: "Technical owners" }).getByRole("listitem")).toHaveText([`${TECH} User`]);
  await expect(card.getByRole("list", { name: "Business owners" }).getByRole("listitem")).toHaveText([`${GROUP} Group`]);
  await checkA11y(page, testInfo, "service detail overview", { strict: true });

  await page.goto(`/services?q=${encodeURIComponent(SHOP)}`);
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: SHOP }) });
  await expect(row.locator("td").nth(3)).toHaveText(TECH);
  await expect(row.locator("td").nth(4)).toHaveText(GROUP);
  await page.goto(`/services?q=${stamp}`);
  await expect(page.locator("table.data tbody tr")).not.toHaveCount(0);
  await checkA11y(page, testInfo, "service list populated", { strict: true });
});

test("3: the picker selects across two pages, keeps members disabled, and adds 3", async ({ page, request }, testInfo) => {
  await apiSend(request, "POST", `/business-services/${ids.shop}/members`, { memberIds: [ids[pg(1)]] });
  await page.goto(`/services/${ids.shop}?tab=members`);
  await expect(page.getByRole("tab", { name: /^Members/ }).filter({ has: page.locator(`[data-count="1"]`) })).toHaveAttribute("aria-selected", "true");
  const add = page.locator(".service-members-actions").getByRole("button", { name: "Add members" });
  await add.click();
  const dialog = picker(page, SHOP);
  await dialog.getByLabel("Search configuration items").fill(PG);
  await expect(dialog.getByText("1–50 of 55")).toBeVisible();

  const first = dialog.getByRole("checkbox", { name: `Select ${pg(1)}` });
  await expect(first).toBeDisabled();
  await expect(dialog.locator("tr", { hasText: pg(1) })).toContainText("Already a member");
  await dialog.getByRole("checkbox", { name: `Select ${pg(2)}` }).check();
  await dialog.getByRole("checkbox", { name: `Select ${pg(3)}` }).check();
  await dialog.getByRole("button", { name: "Next page" }).click();
  await expect(dialog.getByText("51–55 of 55")).toBeVisible();
  await dialog.getByRole("checkbox", { name: `Select ${pg(52)}` }).check();
  // The tray keeps the choice of both pages.
  await expect(dialog.getByRole("heading", { name: "Selected (3)" })).toBeVisible();
  await expect(dialog.locator(".picker-tray li")).toHaveCount(3);

  await dialog.getByRole("button", { name: "Add 3 members" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator(".service-members-live")).toHaveText("3 members added.");
  await expect(add).toBeFocused();
  await expect(page.getByRole("tab", { name: /^Members/ }).filter({ has: page.locator(`[data-count="4"]`) })).toBeVisible();
  await expect(memberLinks(page)).toHaveCount(4);
  await checkA11y(page, testInfo, "service detail members", { strict: true });
});

test("4: a service that includes this one is refused in the tray, and nothing is added", async ({ page, request }, testInfo) => {
  await apiSend(request, "POST", `/business-services/${ids.outer}/members`, { memberIds: [ids.shop] });
  await page.goto(`/services/${ids.shop}?tab=members`);
  await page.locator(".service-members-actions").getByRole("button", { name: "Add members" }).click();
  const dialog = picker(page, SHOP);
  await dialog.getByLabel("Search configuration items").fill(pg(4));
  await dialog.getByRole("checkbox", { name: `Select ${pg(4)}` }).check();
  await dialog.getByLabel("Search configuration items").fill(OUTER);
  await dialog.getByRole("checkbox", { name: `Select ${OUTER}` }).check();
  await dialog.getByRole("button", { name: "Add 2 members" }).click();

  const summary = dialog.locator(".picker-summary");
  await expect(summary).toBeFocused();
  await expect(summary).toContainText("1 of the selected CIs cannot be added.");
  await expect(dialog.locator(".picker-tray li", { hasText: OUTER })).toContainText(`${OUTER} already includes this service. Adding it would create a loop.`);
  await checkA11y(page, testInfo, "member picker with errors", { strict: true });
  // The batch is refused as a whole: the valid CI was not added either.
  expect((await members(request, ids.shop)).page.total).toBe(4);

  await page.keyboard.press("Escape");
  await page.getByRole("dialog", { name: "Discard your selection?" }).getByRole("button", { name: "Discard" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("tab", { name: /^Members/ }).filter({ has: page.locator(`[data-count="4"]`) })).toBeVisible();
});

test("5: remove selected confirms with the number; the row menu removes one", async ({ page, request }) => {
  await page.goto(`/services/${ids.shop}?tab=members`);
  for (const n of [2, 3]) await page.getByRole("checkbox", { name: `Select ${pg(n)}` }).check();
  await page.getByRole("button", { name: /^Remove selected/ }).click();
  const confirm = page.getByRole("dialog", { name: "Remove members" });
  await expect(confirm).toContainText(`Remove 2 members from ${SHOP}? The CIs themselves are not deleted.`);
  await confirm.getByRole("button", { name: "Remove", exact: true }).click();
  await expect(page.locator(".service-members-live")).toHaveText("2 members removed.");
  await expect(page.getByRole("tab", { name: /^Members/ }).filter({ has: page.locator(`[data-count="2"]`) })).toBeVisible();

  await page.getByRole("button", { name: `Actions for ${pg(52)}` }).click();
  await page.getByRole("menu", { name: `Actions for ${pg(52)}` }).getByRole("menuitem", { name: "Remove from service" }).click();
  await expect(confirm).toContainText(`Remove 1 member from ${SHOP}?`);
  await confirm.getByRole("button", { name: "Remove", exact: true }).click();
  await expect(page.locator(".service-members-live")).toHaveText("1 member removed.");
  await expect(page.getByRole("tab", { name: /^Members/ }).filter({ has: page.locator(`[data-count="1"]`) })).toBeVisible();
  expect((await members(request, ids.shop)).data.map((m) => m.ci.id)).toEqual([ids[pg(1)]]);
  // Back to three members for the scenarios below.
  await apiSend(request, "POST", `/business-services/${ids.shop}/members`, { memberIds: [ids[pg(2)], ids[pg(3)]] });
});

test("6: list and Members filters survive a reload, and Back and Forward restore them", async ({ page }) => {
  await page.goto(`/services?q=${stamp}`);
  await page.getByLabel("Owner state").selectOption("none");
  await expect(page).toHaveURL(/ownerState=none/);
  await page.reload();
  await expect(page.getByLabel("Owner state")).toHaveValue("none");
  await expect(page.locator("#svc-q")).toHaveValue(stamp);
  await page.goBack();
  await expect(page.getByLabel("Owner state")).toHaveValue("");
  await page.goForward();
  await expect(page.getByLabel("Owner state")).toHaveValue("none");
  await expect(page.locator("#svc-q")).toHaveValue(stamp);

  await page.goto(`/services/${ids.shop}?tab=members`);
  await page.getByLabel("Search members").fill(pg(2));
  await expect(page).toHaveURL(new RegExp(`[?&]mq=${pg(2)}`));
  await expect(memberLinks(page)).toHaveText([pg(2)]);
  await page.getByLabel("Kind").selectOption("service");
  await expect(page).toHaveURL(/[?&]mkind=service/);
  await expect(page.getByRole("heading", { name: "No members match these filters." })).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Kind")).toHaveValue("service");
  await expect(page.getByLabel("Search members")).toHaveValue(pg(2));
  await page.goBack();
  await expect(page.getByLabel("Kind")).toHaveValue("");
  await expect(memberLinks(page)).toHaveText([pg(2)]);
  await page.goForward();
  await expect(page.getByLabel("Kind")).toHaveValue("service");
  await expect(page.getByRole("heading", { name: "No members match these filters." })).toBeVisible();
});

test("9: a restricted user sees 2 members, the note, and never the hidden CI", async ({ browser }, testInfo) => {
  const page = await signInUi(browser, RESTRICTED);
  try {
    await page.goto(`/services?q=${encodeURIComponent(R_SVC)}`);
    const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: R_SVC }) });
    await expect(row.locator("td").nth(5)).toHaveText("2");

    await page.goto(`/services/${ids.rsvc}?tab=members`);
    await expect(page.getByRole("tab", { name: /^Members/ }).filter({ has: page.locator(`[data-count="2"]`) })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("note")).toHaveText("Members of classes you are not allowed to view are not listed.");
    await expect(memberLinks(page)).toHaveText([WEB1, WEB2]);
    await expect(page.locator(".service-members")).not.toContainText(DB1);
    await checkA11y(page, testInfo, "service detail members restricted", { strict: true });

    const download = page.waitForEvent("download");
    await page.getByRole("button", { name: "Export members (CSV)" }).click();
    const csv = await readFile((await (await download).path())!, "utf8");
    expect(csv).toContain(WEB1);
    expect(csv).toContain(WEB2);
    expect(csv).not.toContain(DB1);

    await page.locator(".service-members-actions").getByRole("button", { name: "Add members" }).click();
    const dialog = picker(page, R_SVC);
    const classes = await dialog.getByLabel("Class", { exact: true }).locator("option").allInnerTexts();
    expect(classes).toContain("Server");
    expect(classes).not.toContain("Database");
    await dialog.getByLabel("Search configuration items").fill(DB1);
    await expect(dialog.getByText("No configuration items match this search.")).toBeVisible();
  } finally {
    await page.context().close();
  }
});

test("10: deleting a nested service names its memberships and parents; the parents lose it", async ({ page, request }, testInfo) => {
  await page.goto(`/services/${ids.inner10}`);
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete business service ${INNER10}?` });
  await expect(dialog).toContainText("Its 2 memberships are removed. The member CIs themselves are not deleted.");
  await expect(dialog).toContainText("It is also part of 2 other business services; it is removed from them.");
  await checkA11y(page, testInfo, "delete dialog", { strict: true });
  await dialog.getByRole("button", { name: "Delete business service" }).click();
  await expect(page).toHaveURL(/\/services$/);

  await page.goto(`/services/${ids.parentA}?tab=members`);
  await expect(page.getByRole("tab", { name: /^Members/ }).filter({ has: page.locator(`[data-count="0"]`) })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("link", { name: INNER10 })).toHaveCount(0);
  expect((await members(request, ids.parentB)).page.total).toBe(0);
  // The member CIs are still there.
  expect((await request.get(`/api/v1/configuration-items/${ids[pg(54)]}`)).ok()).toBeTruthy();
});

test("11: groups admin: create a group, add a member, delete it while it owns services", async ({ page, request }, testInfo) => {
  await page.goto("/admin/groups/new");
  await page.getByLabel("Name").fill(GROUP11);
  await page.getByRole("button", { name: "Create group" }).click();
  await expect(page.getByRole("status").filter({ hasText: `Created group ${GROUP11}.` })).toBeVisible();
  ids.group11 = page.url().split("/").pop()!;

  const add = page.getByRole("combobox", { name: "Add a member" });
  await add.fill(`e2e-acc-theo-${stamp}`);
  await page.getByRole("option", { name: new RegExp(`e2e-acc-theo-${stamp}`) }).click();
  await expect(page.getByTestId("group-member-status")).toHaveText(`e2e-acc-theo-${stamp} was added to the group.`);
  await checkA11y(page, testInfo, "group edit", { strict: true });

  for (const id of [ids.parentA, ids.parentB]) {
    const s = await service(request, id);
    await apiSend(request, "PUT", `/business-services/${id}/owners`, { version: s.version, technical: [], business: [{ kind: "group", id: ids.group11 }] });
  }
  await page.goto("/admin/groups");
  await page.getByRole("searchbox", { name: "Search", exact: true }).fill(GROUP11);
  // The group is on the unfiltered first page too: wait for the search itself to land before the scan.
  await expect(page).toHaveURL(/[?&]q=/);
  await expect(page.locator("table.data.loading")).toHaveCount(0);
  await expect(page.getByRole("link", { name: GROUP11 })).toBeVisible();
  await checkA11y(page, testInfo, "groups list", { strict: true });

  await page.getByRole("link", { name: GROUP11 }).click();
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete group" }).click();
  const dialog = page.getByRole("dialog", { name: `Delete group ${GROUP11}?` });
  await expect(dialog.getByTestId("group-delete-services")).toHaveText("It is owner of 2 business services; it is removed as owner from all of them.");
  await checkA11y(page, testInfo, "group delete dialog", { strict: true });
  await dialog.getByRole("button", { name: "Delete group" }).click();
  await expect(page).toHaveURL(/\/admin\/groups$/);
  expect((await service(request, ids.parentA)).owners.business).toEqual([]);
});

test("12: a second tab saves first; the first tab's save shows the reload message", async ({ page }) => {
  const other = await page.context().newPage();
  try {
    for (const p of [page, other]) {
      await p.goto(`/services/${ids.shop}`);
      await p.getByRole("button", { name: "Edit owners" }).click();
    }
    await pickOwner(other, "Add to Technical owners", TECH2);
    await other.getByRole("button", { name: "Save owners" }).click();
    await expect(other.getByRole("button", { name: "Edit owners" })).toBeVisible();

    await page.getByRole("button", { name: `Remove ${TECH} as technical owner` }).click();
    await page.getByRole("button", { name: "Save owners" }).click();
    const alert = page.getByRole("alert").filter({ hasText: "This service was changed by someone else. Reload to see the current owners." });
    await expect(alert).toBeFocused();
    await alert.getByRole("button", { name: "Reload" }).click();
    await expect(alert).toContainText(TECH2);
  } finally {
    await other.close();
  }
});

test("13: with the locale forced to German, the list, picker and dialogs are German", async ({ page }) => {
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto(`/services?q=${encodeURIComponent(SHOP)}`);
  await expect(page.getByRole("heading", { level: 1, name: "Business-Services" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: /^Kritikalität/ })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Fachlicher Verantwortlicher" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Business-Service anlegen" }).first()).toBeVisible();

  await page.goto(`/services/${ids.shop}?tab=members`);
  await expect(page.getByRole("tab", { name: /^Mitglieder/ }).filter({ has: page.locator(`[data-count="3"]`) })).toHaveAttribute("aria-selected", "true");
  // The table cells too (GH#478): the state and criticality badges come from the catalog.
  await expect(page.getByRole("cell", { name: "Aktiv", exact: true }).first()).toBeVisible();
  await expect(page.getByRole("cell", { name: "Active", exact: true })).toHaveCount(0);
  await expect(page.getByRole("cell", { name: "Not set", exact: true })).toHaveCount(0);
  await page.locator(".service-members-actions").getByRole("button", { name: "Mitglieder hinzufügen" }).click();
  const dialog = page.getByRole("dialog", { name: `Mitglieder zu ${SHOP} hinzufügen` });
  await dialog.getByLabel("Configuration Items suchen").fill(pg(5));
  await dialog.getByRole("checkbox", { name: new RegExp(pg(5)) }).check();
  await expect(dialog.getByRole("button", { name: "1 Mitglied hinzufügen" })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByRole("dialog", { name: "Auswahl verwerfen?" }).getByRole("button", { name: "Verwerfen" }).click();
  await expect(dialog).toBeHidden();

  await page.getByRole("button", { name: "Weitere Aktionen" }).click();
  await page.getByRole("menuitem", { name: "Löschen" }).click();
  const del = page.getByRole("dialog", { name: `Business-Service ${SHOP} löschen?` });
  await expect(del).toContainText("Die Mitglieds-CIs selbst werden nicht gelöscht.");
  await expect(del.getByRole("button", { name: "Business-Service löschen" })).toBeVisible();
  await del.getByRole("button", { name: "Abbrechen" }).click();
  await expect(del).toBeHidden();
});

test("§7.5: the empty service list has no axe violations", async ({ page }, testInfo) => {
  await page.route("**/api/v1/business-services?*", (route) =>
    route.fulfill({ status: 200, json: { data: [], page: { limit: 50, offset: 0, total: 0 }, visibility: "all_classes" } }),
  );
  await page.goto("/services");
  await expect(page.getByText(/^No business services yet\./)).toBeVisible();
  await checkA11y(page, testInfo, "service list empty", { strict: true });
});

// Every CI created here is deleted again: left behind, the 55 servers would push the demo servers off the first
// inventory page and change "the first server by label" for specs that run before this one on a rerun.
test.afterAll(async ({ request }) => {
  const headers = { "X-CSRF-Token": await csrf(request) };
  const cis = ["shop", "outer", "parentA", "parentB", "rsvc", "web1", "web2", "db1", ...Array.from({ length: 55 }, (_, i) => pg(i + 1))];
  for (const key of cis.filter((k) => ids[k])) {
    const res = await request.delete(`/api/v1/configuration-items/${ids[key]}`, { headers });
    expect([204, 404], `DELETE ${key} → ${res.status()}`).toContain(res.status());
  }
  if (ids.group) await request.delete(`/api/v1/admin/groups/${ids.group}`, { headers });
  // The scenario users are throwaway too, so the user lists of later specs stay short.
  for (const u of [OPERATOR, RESTRICTED, `e2e-acc-tess-${stamp}`, `e2e-acc-theo-${stamp}`]) {
    const list = await apiGet<{ data: { id: string; username: string }[] }>(request, `/admin/users?q=${u}`);
    for (const row of list.data.filter((x) => x.username === u)) await request.delete(`/api/v1/admin/users/${row.id}`, { headers });
  }
});
