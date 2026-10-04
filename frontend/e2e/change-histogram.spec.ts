import { checkA11y, classIdByName, createCi, expect, test } from "./support";

// The inventory's change histogram (SHAA-1670 rollout 5d): changes per hour or day to the CIs the
// list's filters match, from GET /configuration-items/change-histogram. The admin has audit.view.

test("inventory: the change histogram counts the filtered CIs' changes, by range and by day", async ({ page, request }, testInfo) => {
  const stamp = `e2e-hist-${Date.now().toString(36)}`;
  await createCi(request, await classIdByName(request, "Server"), `${stamp}-a`);
  await createCi(request, await classIdByName(request, "Server"), `${stamp}-b`);

  await page.goto(`/cis?q=${stamp}`);
  await expect(page.locator("table.data tbody tr")).toHaveCount(2);
  const strip = page.getByRole("region", { name: "Changes", exact: true });
  await expect(strip.getByRole("button", { name: "Changes", exact: true })).toHaveAttribute("aria-expanded", "true");
  // The filters reach the API: only the two CIs created above are counted.
  await expect(strip.locator("#histogram-summary")).toHaveText(/^2 changes in the last 7 days · peak 2 per hour, /);
  for (const name of ["Updated", "Created", "Status changed"]) await expect(strip.getByRole("listitem").filter({ hasText: name })).toBeVisible();

  // The plot is one tab stop whose value text reads the bar under it; it starts on the current hour.
  const plot = strip.getByRole("slider", { name: "Changes per hour" });
  await plot.focus();
  await expect(plot).toHaveAttribute("aria-valuemax", "167");
  await expect(plot).toHaveAttribute("aria-valuetext", /: 0 updated · 2 created · 0 status changes$/);
  await plot.press("Home");
  await expect(plot).toHaveAttribute("aria-valuenow", "0");

  // Every count, without a pointer: the values table lists the non-empty buckets.
  await strip.getByRole("button", { name: "Show values" }).click();
  const values = strip.getByRole("table", { name: "Changes by time" });
  await expect(values.locator("tbody tr")).toHaveCount(1);
  await expect(values.locator("tbody tr td")).toHaveText(["0", "2", "0", "2"]);
  await strip.getByRole("button", { name: "Hide values" }).click();
  await expect(values).toHaveCount(0);

  // 30 days by day; Enter on today's bar shows it by hour, and Back returns to the range.
  await strip.getByRole("radio", { name: "30 d" }).check({ force: true });
  const days = strip.getByRole("slider", { name: "Changes per day" });
  await expect(days).toHaveAttribute("aria-valuemax", "29");
  await days.focus();
  await days.press("End");
  await days.press("Enter");
  await expect(strip.getByRole("slider", { name: "Changes per hour" })).toBeVisible();
  await expect(strip.locator("#histogram-summary")).toHaveText(/^2 changes on /);
  await strip.getByRole("button", { name: "Back to 30 days" }).click();
  await expect(strip.getByRole("slider", { name: "Changes per day" })).toBeVisible();

  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await checkA11y(page, testInfo, `change-histogram-${colorScheme}`, { include: ".histogram", strict: true });
  }

  // The range and a closed strip are remembered in this browser.
  await page.reload();
  await expect(strip.getByRole("radio", { name: "30 d" })).toBeChecked();
  await strip.getByRole("button", { name: "Changes", exact: true }).click();
  await expect(strip.getByRole("slider")).toHaveCount(0);
  await page.reload();
  await expect(strip.getByRole("button", { name: "Changes", exact: true })).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator("table.data tbody tr")).toHaveCount(2);
});

test("inventory: no change histogram on a narrow screen", async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 900 });
  await page.goto("/cis");
  await expect(page.locator("table.data tbody tr").first()).toBeVisible();
  await expect(page.getByRole("region", { name: "Changes", exact: true })).toHaveCount(0);
});
