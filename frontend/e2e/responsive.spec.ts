import type { Page } from "@playwright/test";
import { snap, expect, test } from "./support";

// GH#182: the header and sidebar must fit narrow windows (a laptop split screen, a 768 px tablet)
// for an administrator, who sees every header control.

/** Asserts the search field is usable, "New CI" is fully on screen and nothing scrolls sideways. */
async function expectHeaderFits(page: Page) {
  const search = await page.locator("#global-search").boundingBox();
  expect(search?.width ?? 0).toBeGreaterThanOrEqual(150);
  const viewport = page.viewportSize()!;
  const newCi = await page.getByRole("link", { name: "New CI", exact: true }).boundingBox();
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

  // User menu: the theme select and "Sign out" open from the user's name; the profile badge is gone.
  const header = page.locator(".shell-header");
  await expect(header.getByText("Administrator", { exact: true })).toHaveCount(0);
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
  await page.getByRole("link", { name: "New CI", exact: true }).click();
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

test("desktop: user name, badge, theme and sign-out stay in the header", async ({ page }) => {
  await page.goto("/cis");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  const header = page.locator(".shell-header");
  await expect(header.getByText("Administrator", { exact: true })).toBeVisible();
  await expect(page.getByLabel("Theme")).toBeVisible();
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();
  await expect(page.getByRole("button", { name: /^Signed in as/ })).toHaveCount(0);
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
