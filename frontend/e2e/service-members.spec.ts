import type { Browser, Page } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { apiGet, apiSend, checkA11y, classIdByName, createCi, expect, snap, test } from "./support";

// Business services U3 (SHAA-934, spec SHAA-927 §5.3, §5.5–5.7, §2): a service's Members tab, the "Add members"
// picker, the member CSV, the "Part of business services" panel and the Impact tab's pinned services.
// Under a stamp: two servers and a third, an inner service and an outer service that includes the inner one.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const N = (s: string) => `svc-${s}-${stamp}`;
const ids: Record<string, string> = {};
const VIEWER = `e2e-svc-viewer-${stamp}`;
const PASSWORD = "service-viewer-password-123";

test.beforeAll(async ({ request }) => {
  const settings = await apiGet<{ classId: string }>(request, "/settings/business-services");
  const server = await classIdByName(request, "Server");
  for (const s of ["srv-1", "srv-2", "srv-3"]) ids[s] = (await createCi(request, server, N(s))).id;
  ids.inner = (await createCi(request, settings.classId, N("inner"))).id;
  ids.outer = (await createCi(request, settings.classId, N("outer"))).id;
  await apiSend(request, "POST", `/business-services/${ids.outer}/members`, { memberIds: [ids.inner] });

  // A viewer of business services and nothing else: the servers are members they may not see.
  const profile = await apiSend<{ id: string }>(request, "POST", "/admin/profiles", {
    name: `E2E service viewers ${stamp}`,
    globalPermissions: [],
    classPermissions: [{ classId: settings.classId, view: true, create: false, edit: false, delete: false }],
  });
  await apiSend(request, "POST", "/admin/users", { username: VIEWER, displayName: VIEWER, password: PASSWORD, profileIds: [profile.id] });
});

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

const picker = (page: Page) => page.getByRole("dialog", { name: `Add members to ${N("inner")}` });
const memberNames = (page: Page) => page.locator(".service-members tbody tr td:nth-child(2) a").allInnerTexts();

test("Members tab: empty, then two members added through the picker", async ({ page }, testInfo) => {
  await page.goto(`/cis/${ids.inner}?tab=members`);
  await expect(page.getByRole("tab", { name: "Members (0)" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("heading", { name: "This service has no members yet." })).toBeVisible();

  const add = page.locator(".service-members-actions").getByRole("button", { name: "Add members" });
  await add.click();
  const dialog = picker(page);
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAttribute("aria-modal", "true");
  await expect(dialog.getByLabel("Search configuration items")).toBeFocused();
  await dialog.getByLabel("Search configuration items").fill(`srv-1-${stamp}`);
  await dialog.getByRole("checkbox", { name: `Select ${N("srv-1")}` }).check();
  await dialog.getByLabel("Search configuration items").fill(`srv-2-${stamp}`);
  await dialog.getByRole("checkbox", { name: `Select ${N("srv-2")}` }).check();
  // The tray keeps the choice across searches.
  await expect(dialog.getByRole("heading", { name: "Selected (2)" })).toBeVisible();
  await checkA11y(page, testInfo, "member-picker", { include: "dialog.member-picker" });
  await snap(page, "member-picker");

  // Esc with a selection asks first; keeping it leaves the dialog open.
  await page.keyboard.press("Escape");
  const discard = page.getByRole("dialog", { name: "Discard your selection?" });
  await expect(discard).toBeVisible();
  await discard.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeVisible();

  await dialog.getByRole("button", { name: "Add 2 members" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator(".service-members-live")).toHaveText("2 members added.");
  await expect(add).toBeFocused();
  await expect(page.getByRole("tab", { name: "Members (2)" })).toBeVisible();
  expect((await memberNames(page)).sort()).toEqual([N("srv-1"), N("srv-2")]);
  await checkA11y(page, testInfo, "members-tab", { include: ".service-members" });
  await snap(page, "members-tab");
});

test("picker: already a member, and refused CIs stay in the open dialog with their reasons", async ({ page }) => {
  await page.goto(`/cis/${ids.inner}?tab=members`);
  await page.locator(".service-members-actions").getByRole("button", { name: "Add members" }).click();
  const dialog = picker(page);
  await dialog.getByLabel("Search configuration items").fill(stamp);
  await expect(dialog.getByRole("checkbox", { name: `Select ${N("srv-1")}` })).toBeDisabled();
  await expect(dialog.locator("tr", { hasText: N("srv-1") })).toContainText("Already a member");
  await dialog.getByRole("checkbox", { name: `Select ${N("srv-3")}` }).check();
  // Never pre-judged by the client: the service itself, and the service that includes it.
  await dialog.getByRole("checkbox", { name: `Select ${N("inner")}` }).check();
  await dialog.getByRole("checkbox", { name: `Select ${N("outer")}` }).check();
  await dialog.getByRole("button", { name: "Add 3 members" }).click();

  const summary = dialog.locator(".picker-summary");
  await expect(summary).toBeFocused();
  await expect(summary).toContainText("2 of the selected CIs cannot be added.");
  const tray = dialog.locator(".picker-tray");
  await expect(tray.locator("li", { hasText: N("inner") })).toContainText("A service cannot be a member of itself.");
  await expect(tray.locator("li", { hasText: N("outer") })).toContainText(`${N("outer")} already includes this service. Adding it would create a loop.`);
  await snap(page, "member-picker-422");

  await tray.getByRole("button", { name: `Remove ${N("inner")} from the selection` }).click();
  await tray.getByRole("button", { name: `Remove ${N("outer")} from the selection` }).click();
  await dialog.getByRole("button", { name: "Add 1 member" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator(".service-members-live")).toHaveText("1 member added.");
  await expect(page.getByRole("tab", { name: "Members (3)" })).toBeVisible();
});

test("filters live in the URL and survive a reload", async ({ page }) => {
  await page.goto(`/cis/${ids.inner}?tab=members`);
  await page.getByLabel("Search members").fill(`srv-3-${stamp}`);
  await expect(page).toHaveURL(new RegExp(`[?&]mq=srv-3-${stamp}`));
  await page.getByLabel("Kind").selectOption("ci");
  await expect(page).toHaveURL(/[?&]mkind=ci/);
  await page.getByRole("button", { name: /^Added/ }).click();
  await expect(page).toHaveURL(/[?&]msort=addedAt/);
  await page.reload();
  await expect(page.getByRole("tab", { name: "Members (3)" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByLabel("Search members")).toHaveValue(`srv-3-${stamp}`);
  await expect(page.getByLabel("Kind")).toHaveValue("ci");
  expect(await memberNames(page)).toEqual([N("srv-3")]);

  await page.getByLabel("Kind").selectOption("service");
  await expect(page.getByRole("heading", { name: "No members match these filters." })).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).last().click();
  await expect.poll(() => memberNames(page)).toHaveLength(3);
});

test("remove selected and the row menu confirm with the number", async ({ page }) => {
  await page.goto(`/cis/${ids.inner}?tab=members`);
  const removeSelected = page.getByRole("button", { name: /^Remove selected/ });
  await expect(removeSelected).toBeDisabled();
  await page.getByRole("checkbox", { name: `Select ${N("srv-3")}` }).check();
  await removeSelected.click();
  const confirm = page.getByRole("dialog", { name: "Remove members" });
  await expect(confirm).toContainText(`Remove 1 member from ${N("inner")}? The CIs themselves are not deleted.`);
  await confirm.getByRole("button", { name: "Remove", exact: true }).click();
  await expect(page.locator(".service-members-live")).toHaveText("1 member removed.");
  await expect(page.getByRole("tab", { name: "Members (2)" })).toBeVisible();

  // The row menu: Open, Impact analysis, Remove from service; Esc returns to its button.
  const menuButton = page.getByRole("button", { name: `Actions for ${N("srv-2")}` });
  await menuButton.click();
  const menu = page.getByRole("menu", { name: `Actions for ${N("srv-2")}` });
  await expect(menu.getByRole("menuitem")).toHaveText(["Open", "Impact analysis", "Remove from service"]);
  await expect(menu.getByRole("menuitem", { name: "Open" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: "Impact analysis" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
  await expect(menuButton).toBeFocused();
});

test("export members (CSV) downloads the visible members", async ({ page }) => {
  await page.goto(`/cis/${ids.inner}?tab=members`);
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export members (CSV)" }).click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(/^service-members-.+\.csv$/);
  const csv = await readFile((await file.path())!, "utf8");
  expect(csv).toContain("ci_id");
  expect(csv).toContain(N("srv-1"));
  expect(csv).toContain(N("srv-2"));
  expect(csv).not.toContain(N("srv-3"));
});

test("a member's Overview shows the services it is part of, directly and through nesting", async ({ page }, testInfo) => {
  await page.goto(`/cis/${ids["srv-1"]}`);
  const panel = page.locator("section.part-of-services");
  await expect(panel.getByRole("heading", { name: "Part of business services" })).toBeVisible();
  // Direct first, then by nesting depth.
  const rows = panel.locator("tbody tr");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0).locator("td").first()).toHaveText(N("inner"));
  await expect(rows.nth(0)).toContainText("direct");
  const nested = rows.nth(1);
  await expect(nested.locator("td").first()).toHaveText(N("outer"));
  await expect(nested).toContainText(`via ${N("inner")}`);
  await nested.locator("td").last().getByRole("link", { name: N("inner") }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ids.inner}$`));
  await checkA11y(page, testInfo, "part-of", { include: "section.part-of-services" });

  // A CI that is part of none shows nothing at all.
  await page.goto(`/cis/${ids["srv-3"]}`);
  await expect(page.getByRole("heading", { level: 1, name: N("srv-3") })).toBeVisible();
  await expect(page.locator("section.part-of-services")).toHaveCount(0);
});

test("Impact tab pins the affected business services, most critical first", async ({ page }) => {
  await page.goto(`/cis/${ids["srv-1"]}/impact`);
  const section = page.locator("section.impact-services");
  await expect(section.getByRole("heading", { name: /Affected business services \(2\)/ })).toBeVisible();
  await expect(section.locator("tbody tr td:first-child a")).toHaveText([N("inner"), N("outer")]);
  await expect(section.locator("tbody tr").first()).toContainText("1");
  await snap(page, "impact-services");
  // Collapsible, open by default.
  const toggle = section.getByRole("button", { name: /Affected business services/ });
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await toggle.click();
  await expect(section.locator("table")).toBeHidden();
});

test("a restricted viewer sees only the members they may view, the static note, and no edit actions", async ({ browser }) => {
  const page = await signInUi(browser, VIEWER);
  try {
    await page.goto(`/cis/${ids.inner}?tab=members`);
    // The servers are hidden: the count is the visible one, never "N of M".
    await expect(page.getByRole("tab", { name: "Members (0)" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("heading", { name: "This service has no members yet." })).toBeVisible();
    await expect(page.getByRole("note")).toHaveText("Members of classes you are not allowed to view are not listed.");
    await expect(page.getByRole("button", { name: "Add members" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: /^Remove selected/ })).toHaveCount(0);

    await page.goto(`/cis/${ids.outer}?tab=members`);
    await expect(page.locator(".service-members tbody tr td:first-child a")).toHaveText([N("inner")]);
    await expect(page.locator(".service-members tbody")).toContainText("Business service");
  } finally {
    await page.context().close();
  }
});
