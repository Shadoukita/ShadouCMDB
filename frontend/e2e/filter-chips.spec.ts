import type { Locator } from "@playwright/test";
import { apiSend, classIdByName, expect, test } from "./support";

// The inventory's filter chips (GH#787): the key ("Class:", "Needs attention:") keeps its natural
// width, a long value gives way with an ellipsis, and the full value stays in the chip's text and title.
const stamp = Date.now().toString(36);

async function expectKeyClear(chip: Locator) {
  const key = chip.locator(".key");
  const value = chip.locator("bdi");
  const k = await key.evaluate((el) => ({ right: el.getBoundingClientRect().right, client: el.clientWidth, scroll: el.scrollWidth }));
  const left = await value.evaluate((el) => el.getBoundingClientRect().left);
  // The key is not squeezed below its text, and the value starts after it.
  expect(k.client).toBeGreaterThanOrEqual(k.scroll);
  expect(k.right).toBeLessThanOrEqual(left);
}

test("inventory: a filter chip with a long value keeps its key and truncates the value (GH#787)", async ({ page, request }) => {
  const name = `E2E chip ${stamp} with a class name long enough to be cut off in the filter chip of the inventory list`;
  const cls = await apiSend<{ id: string }>(request, "POST", "/ci-classes", {
    key: `e2e_chip_${stamp}`,
    name,
    parentId: await classIdByName(request, "Hardware"),
  });

  await page.goto(`/cis?classId=${cls.id}&quality=no_owner`);
  const chip = page.locator("[data-chip='class']");
  await expect(chip.locator(".key")).toHaveText("Class:");
  const value = chip.locator("bdi");
  await expect(value).toHaveText(name);
  await expect(value).toHaveAttribute("title", name);
  await expectKeyClear(chip);
  // Cut off with an ellipsis, not wrapped or overflowing into the clear button.
  expect(await value.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
  expect(await value.evaluate((el) => getComputedStyle(el).textOverflow)).toBe("ellipsis");
  const clear = chip.locator(".chip-clear");
  expect((await value.boundingBox())!.x + (await value.boundingBox())!.width).toBeLessThanOrEqual((await clear.boundingBox())!.x);

  await expectKeyClear(page.locator("[data-chip='quality']"));

  // A narrow window: the chip shrinks to its row and the key still reads in full.
  await page.setViewportSize({ width: 480, height: 800 });
  await expectKeyClear(chip);
  const row = await page.locator(".inventory-filters .filter-chips").boundingBox();
  const box = await chip.boundingBox();
  expect(box!.x + box!.width).toBeLessThanOrEqual(row!.x + row!.width + 1);
});
