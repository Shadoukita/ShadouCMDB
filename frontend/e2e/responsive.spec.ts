import type { Page } from "@playwright/test";
import { snap, expect, openUserMenu, test } from "./support";

// GH#182: the header and sidebar must fit narrow windows (a laptop split screen, a 768 px tablet)
// for an administrator, who sees every header control.

/** Asserts the search field is usable, "New CI" is fully on screen and nothing scrolls sideways. */
async function expectHeaderFits(page: Page) {
  const search = await page.locator("#global-search").boundingBox();
  expect(search?.width ?? 0).toBeGreaterThanOrEqual(150);
  const viewport = page.viewportSize()!;
  const newCi = await page.getByRole("banner").getByRole("link", { name: "New CI", exact: true }).boundingBox();
  expect(newCi).not.toBeNull();
  expect(newCi!.x).toBeGreaterThanOrEqual(0);
  expect(newCi!.y).toBeGreaterThanOrEqual(0);
  expect(newCi!.x + newCi!.width).toBeLessThanOrEqual(viewport.width);
  expect(newCi!.y + newCi!.height).toBeLessThanOrEqual(viewport.height);
  const { scrollWidth, innerWidth } = await page.evaluate(() => ({
    scrollWidth: document.documentElement.scrollWidth,
    innerWidth: window.innerWidth,
  }));
  expect(scrollWidth).toBeLessThanOrEqual(innerWidth);
}

test("768 px: header fits, sidebar is a drawer, theme and sign-out sit in the user menu", async ({ page }) => {
  await page.setViewportSize({ width: 768, height: 900 });
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expectHeaderFits(page);
  await snap(page, "responsive-768-header");

  // Sidebar: hidden behind a toggle; opening it shows the class nav, following a link closes it.
  const nav = page.getByRole("navigation", { name: "Main" });
  const toggle = page.getByRole("button", { name: "Open navigation" });
  await expect(nav).toBeHidden();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await toggle.click();
  await expect(page.getByRole("button", { name: "Close navigation" })).toHaveAttribute("aria-expanded", "true");
  await expect(nav).toBeVisible();
  await snap(page, "responsive-768-nav-open");
  await page.keyboard.press("Escape");
  await expect(nav).toBeHidden();
  await expect(toggle).toBeFocused();
  await toggle.click();
  await nav.getByRole("link", { name: /^Server \d+$/ }).click();
  await expect(page).toHaveURL(/\/cis\?classId=/);
  await expect(nav).toBeHidden();

  // User menu: the theme select, the profile badge and "Sign out" open from the user's initials.
  const header = page.locator(".shell-header");
  await expect(header.getByText("Administrator", { exact: true })).toBeHidden();
  await expect(page.getByLabel("Theme")).toBeHidden();
  const who = page.getByRole("button", { name: /^Signed in as/ });
  await who.click();
  await expect(who).toHaveAttribute("aria-expanded", "true");
  await expect(page.getByLabel("Theme")).toBeVisible();
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();
  await expect(header.getByRole("link", { name: "My account" })).toBeVisible();
  await snap(page, "responsive-768-user-menu");
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Theme")).toBeHidden();
  await expect(who).toBeFocused();

  // "New CI" is an icon button that keeps its accessible name and still opens the form.
  await page.getByRole("banner").getByRole("link", { name: "New CI", exact: true }).click();
  await expect(page).toHaveURL(/\/cis\/new/);
});

test("900 px: sidebar stays, header still fits", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Main" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Open navigation" })).toHaveCount(0);
  await expectHeaderFits(page);
});

test("desktop: the user block at the bottom of the rail holds the badge, theme, density and sign-out; the nav collapses to a rail", async ({ page }) => {
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expectHeaderFits(page);
  // Step 12b: the user block sits at the bottom of the rail, not in the header.
  const rail = page.getByRole("navigation", { name: "Main" });
  const who = rail.getByRole("button", { name: /^Signed in as \S/ });
  await expect(page.locator(".shell-header .user-menu")).toHaveCount(0);
  await expect(who.locator(".who-name")).toBeVisible();
  await expect(who.locator(".who-role")).toHaveText("Administrator");
  await expect(page.getByLabel("Theme")).toBeHidden();
  await who.click();
  await expect(page.locator(".user-menu-panel").getByText("Administrator", { exact: true })).toBeVisible();
  await expect(page.getByLabel("Theme")).toBeVisible();
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();
  await snap(page, "responsive-desktop-user-menu");

  // Density: comfortable is remembered in this browser; standard is the stylesheet default.
  await page.getByLabel("Density").selectOption("comfortable");
  await expect(page.locator("html")).toHaveAttribute("data-density", "comfortable");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-density", "comfortable");
  await openUserMenu(page);
  await page.getByLabel("Density").selectOption("standard");
  await expect(page.locator("html")).not.toHaveAttribute("data-density", /./);
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Theme")).toBeHidden();

  // The nav collapses to an icon rail: pages keep their names (tooltip and accessible name), classes hide.
  const nav = page.getByRole("navigation", { name: "Main" });
  await expect(nav.getByRole("link", { name: /^Server \d+$/ })).toBeVisible();
  await nav.getByRole("button", { name: "Collapse sidebar" }).click();
  await expect(nav.getByRole("link", { name: /^Server/ })).toHaveCount(0);
  const inventory = nav.getByRole("link", { name: "All configuration items", exact: true });
  await expect(inventory).toBeVisible();
  await expect(inventory).toHaveAttribute("title", "All configuration items");
  expect((await nav.boundingBox())!.width).toBeLessThanOrEqual(56);
  // The collapsed user block shows the initials and keeps the user's name as its accessible name.
  await expect(nav.getByRole("button", { name: /^Signed in as \S/ })).toBeVisible();
  await snap(page, "responsive-desktop-rail");
  await page.reload();
  await expect(nav.getByRole("button", { name: "Expand sidebar" })).toHaveAttribute("aria-expanded", "false");
  await nav.getByRole("button", { name: "Expand sidebar" }).click();
  await expect(nav.getByRole("link", { name: /^Server \d+$/ })).toBeVisible();
  await expectHeaderFits(page);
});

// GH#363: at 320 px (WCAG 1.4.10 reflow) the user's name in the header pushed the page to 397 px.
test("320 px: header reflows, the user menu shows initials and names the user when opened", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 640 });
  for (const path of ["/", "/cis"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expectHeaderFits(page);
  }
  const who = page.getByRole("button", { name: /^Signed in as \S/ });
  await expect(who.locator(".who-initials")).toBeVisible();
  await expect(who.locator(".who-initials")).not.toBeEmpty();
  await who.click();
  await expect(page.locator(".user-menu-panel").getByText(/^Signed in as \S/)).toBeVisible();
  const panel = await page.locator(".user-menu-panel").boundingBox();
  expect(panel!.x).toBeGreaterThanOrEqual(0);
  expect(panel!.x + panel!.width).toBeLessThanOrEqual(320);
  await snap(page, "responsive-320-user-menu");
});

// Audit A1/A2 (SHAA-1670, rollout 8): Administration has no second navigation column beside the page. Its
// sections sit in the rail under "Administration", beside the page only while the rail is collapsed, and
// in the drawer under 820 px, so a 320 px window does not scroll sideways.
test("Administration: sections in the rail, beside the page when collapsed, in the drawer at 320 px", async ({ page }) => {
  await page.goto("/admin/users");
  const main = page.getByRole("navigation", { name: "Main" });
  const sections = page.getByRole("navigation", { name: "Administration" });
  await expect(sections).toHaveCount(1);
  await expect(main.getByRole("navigation", { name: "Administration" })).toBeVisible();
  await expect(sections.getByRole("link", { name: "Users", exact: true })).toHaveAttribute("aria-current", "page");
  await sections.getByRole("link", { name: "Audit log" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Audit log");
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toContainText("System");

  await main.getByRole("button", { name: "Collapse sidebar" }).click();
  await expect(sections).toHaveCount(1);
  await expect(page.getByRole("main").getByRole("navigation", { name: "Administration" })).toBeVisible();
  await snap(page, "responsive-admin-rail-collapsed");
  await main.getByRole("button", { name: "Expand sidebar" }).click();
  await expect(main.getByRole("navigation", { name: "Administration" })).toBeVisible();

  await page.setViewportSize({ width: 320, height: 800 });
  await page.goto("/admin/users");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Users");
  await expect(sections).toBeHidden();
  const { scrollWidth, innerWidth } = await page.evaluate(() => ({ scrollWidth: document.documentElement.scrollWidth, innerWidth: window.innerWidth }));
  expect(scrollWidth).toBeLessThanOrEqual(innerWidth);
  await page.getByRole("button", { name: "Open navigation" }).click();
  await expect(sections).toBeVisible();
  await sections.getByRole("link", { name: "Permission profiles" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Permission profiles");
  await expect(sections).toBeHidden();
  await snap(page, "responsive-admin-320");
});
