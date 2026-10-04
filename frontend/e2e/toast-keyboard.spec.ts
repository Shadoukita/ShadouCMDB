import type { Page } from "@playwright/test";
import { classIdByName, createCi, expect, test } from "./support";

// Toasts by keyboard (GH#596, GH#597): F8 reaches the newest toast before it closes and goes back,
// Escape closes the focused toast, and focus never falls to <body> when a focused toast goes away.
const stamp = Date.now();
const name = `toast-kbd-${stamp}`;
let ciId = "";

test.beforeAll(async ({ request }) => {
  ciId = (await createCi(request, await classIdByName(request, "Server"), name)).id;
});

const toasts = (page: Page) => page.locator(".toast-host .toast");
const activeTag = (page: Page) => page.evaluate(() => document.activeElement?.tagName);

/** Changes the CPU cores and saves, which shows a "Saved …" toast. */
async function save(page: Page, cores: string) {
  await page.locator("#attr-cpu_cores").fill(cores);
  await page.getByRole("region", { name: "Unsaved changes" }).getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("region", { name: "Unsaved changes" })).toHaveCount(0);
}

test("F8 reaches the newest toast and back; Escape closes it and focus stays in place", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  await save(page, "4");
  await save(page, "8");
  await expect(toasts(page)).toHaveCount(2);

  // From a field on the page, F8 jumps to the newest toast; its clock stops while it has focus.
  const field = page.locator("#attr-hostname");
  await field.focus();
  await page.keyboard.press("F8");
  const close = page.getByRole("button", { name: "Dismiss notification" });
  await expect(close.last()).toBeFocused();
  await expect(close.last()).toHaveAttribute("aria-keyshortcuts", "Escape");
  await page.waitForTimeout(6500);
  await expect(toasts(page)).toHaveCount(2);

  // F8 again goes back to the field.
  await page.keyboard.press("F8");
  await expect(field).toBeFocused();

  // Escape closes the focused toast; focus moves to the one that is left, then back to the field.
  await page.keyboard.press("F8");
  await page.keyboard.press("Escape");
  await expect(toasts(page)).toHaveCount(1);
  await expect(close).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(toasts(page)).toHaveCount(0);
  await expect(field).toBeFocused();

  // With no toast on screen, F8 does nothing.
  await page.keyboard.press("F8");
  await expect(field).toBeFocused();
});

test("closing the last toast with the mouse after tabbing in does not drop focus to <body>", async ({ page }) => {
  await page.goto(`/cis/${ciId}`);
  await save(page, "12");
  await page.keyboard.press("F8");
  await page.getByRole("button", { name: "Dismiss notification" }).click();
  await expect(toasts(page)).toHaveCount(0);
  expect(await activeTag(page)).not.toBe("BODY");
});
