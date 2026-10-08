import { apiGet, checkA11y, chooseTheme, expect, resetUiSettings, test } from "./support";

// The dashboard in the reference-mockup look (design document §0, step 12e): the date and a greeting, the
// period switch (in the URL), KPI cards with their delta chips and bars (gaps G1 and G2), the stacked changes
// chart, CIs by class as bars in the categorical series colours, and the recent activity from the audit log.
// Read-only: the figures are compared with the API, so the spec does not depend on what other specs created.

test("dashboard: greeting, period switch, KPI cards, changes chart, class bars and recent activity", async ({ page, request }, testInfo) => {
  await resetUiSettings(request); // the built-in widgets
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1, name: /^Dashboard: Good (morning|afternoon|evening)$/ })).toBeVisible();

  // KPI cards: the figures the API counts, with the move over the period in the chip's text.
  const stats = page.getByRole("region", { name: "Dashboard figures" });
  const total = (await apiGet<{ page: { total: number } }>(request, "/configuration-items?limit=1")).page.total;
  await expect(stats.locator('[data-stat="total"] .value')).toHaveText(total.toLocaleString("en-US"));
  await expect(stats.locator('[data-stat="total"] .delta-chip')).toHaveText(/^[+−±][\d,]+.* since .+$/);
  await expect(stats.locator('[data-stat="total"] .kpi-bars > span')).toHaveCount(14);
  const completeness = await apiGet<{ overall: { items: number; completeItems: number } }>(request, "/configuration-items/completeness");
  const pct = completeness.overall.items ? Math.round((completeness.overall.completeItems / completeness.overall.items) * 1000) / 10 : 100;
  await expect(stats.locator('[data-stat="complete"] .value')).toHaveText(`${pct.toLocaleString("en-US")}%`);

  // The period switch: 14 days by default; 90 days goes into the URL and survives a reload.
  const period = page.getByRole("radiogroup", { name: "Period" });
  await expect(period.getByRole("radio", { name: "14 days" })).toBeChecked();
  await period.getByText("90 days").click();
  await expect(page).toHaveURL(/[?&]period=90d\b/);
  await expect(page.getByRole("heading", { name: "Changes over 90 days" })).toBeVisible();
  await expect(stats.locator('[data-stat="total"] .kpi-bars > span')).toHaveCount(14); // 13 ISO weeks and the current one
  await page.reload();
  await expect(page.getByRole("radiogroup", { name: "Period" }).getByRole("radio", { name: "90 days" })).toBeChecked();
  await page.getByRole("radiogroup", { name: "Period" }).getByText("24 h").click();
  await expect(page).toHaveURL(/[?&]period=24h\b/);
  await expect(page.getByRole("heading", { name: "Changes over 24 hours" })).toBeVisible();

  // The changes chart: a picture named by its summary, every count in the values table.
  const chart = page.getByTestId("changes-chart");
  await expect(chart.getByRole("img")).toHaveAttribute("aria-label", /(change|No changes)/);
  await expect(chart.getByRole("listitem")).toHaveText(["Updated", "Created", "Status changed"]);
  await chart.getByRole("button", { name: "Show values" }).click();
  await expect(chart.getByRole("table", { name: "Changes by time" }).locator("tbody tr")).toHaveCount(24);

  // CIs by class: largest first, each a link to its inventory, the first in the first series colour.
  const byClass = page.locator('[data-widget="by_class"]');
  await expect(byClass.getByRole("link", { name: "Open inventory" })).toHaveAttribute("href", "/cis");
  await expect(byClass.locator(".count-value .spinner")).toHaveCount(0);
  const counts = await byClass.locator(".count-value").evaluateAll((els) => els.map((e) => Number((e.firstChild?.textContent ?? "").replace(/\D/g, ""))));
  expect(counts).toEqual([...counts].sort((a, b) => b - a));
  await expect(byClass.locator("li").first()).toHaveClass(/\bviz-1\b/);

  // Recent activity: the newest CI change in the audit log comes first.
  const newest = (await apiGet<{ data: { newValue: { label?: string } | null; oldValue: { label?: string } | null }[] }>(
    request,
    "/audit-log?entityType=configuration_items&sort=-occurredAt&limit=1",
  )).data[0];
  const recent = page.locator('[data-widget="recent"]');
  await expect(recent.getByRole("link", { name: "View audit log" })).toBeVisible();
  if (newest) await expect(recent.locator("tbody tr").first().locator("td").first()).toHaveText((newest.newValue ?? newest.oldValue)?.label ?? "");

  await checkA11y(page, testInfo, "dashboard-light");
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "dashboard-dark");
  await chooseTheme(page, "");
});

// "Needs attention" (gap G3): one row per data-quality check the caller can use, its count from the API, each a
// link to the inventory filtered to the CIs it finds. A check no class the caller may view is set up for is left out.
test("dashboard: Needs attention lists the data-quality checks and drills down into the inventory", async ({ page, request }, testInfo) => {
  await resetUiSettings(request);
  type Check = { key: string; count: number; configured: boolean; filter: { quality: string; endOfLifeWithinDays: number | null } };
  const { checks } = await apiGet<{ checks: Check[] }>(request, "/configuration-items/data-quality");
  const shown = checks.filter((c) => c.configured);
  const titles: Record<string, RegExp> = {
    no_owner: /^Without an owner$/,
    end_of_life: /^End of life within \d+ days?$/,
    no_relationships: /^No relationships$/,
    pending_approval: /^Pending approvals$/,
  };

  await page.goto("/");
  const panel = page.getByRole("region", { name: "Needs attention" });
  await expect(panel).toBeVisible();
  await expect(panel.getByText(`${shown.length} checks`)).toBeVisible();
  await expect(panel.getByRole("listitem")).toHaveCount(shown.length);
  for (const c of checks) {
    const row = panel.locator(`[data-check="${c.key}"]`);
    if (!c.configured) {
      await expect(row).toHaveCount(0);
      continue;
    }
    await expect(row.locator(".attention-count")).toHaveText(c.count.toLocaleString("en-US"));
    await expect(row.locator(".attention-title")).toHaveText(titles[c.key]);
    const href = await row.getByRole("link").getAttribute("href");
    const url = new URL(href ?? "", "http://x");
    expect(url.pathname).toBe("/cis");
    expect(url.searchParams.get("quality")).toBe(c.filter.quality);
    expect(url.searchParams.get("endOfLifeWithinDays")).toBe(c.filter.endOfLifeWithinDays === null ? null : String(c.filter.endOfLifeWithinDays));
  }

  // The panel beside the recent activity, in one row of the grid.
  const recent = await page.locator('[data-widget="recent"]').boundingBox();
  const aside = await panel.boundingBox();
  expect(recent && aside && Math.abs(recent.y - aside.y) < 2 && aside.x > recent.x).toBe(true);

  await checkA11y(page, testInfo, "dashboard-attention-light", { include: '[data-testid="needs-attention"]' });
  await chooseTheme(page, "dark");
  await checkA11y(page, testInfo, "dashboard-attention-dark", { include: '[data-testid="needs-attention"]' });
  await chooseTheme(page, "");

  // Drill-down: the inventory lists exactly the CIs the check counted, under a removable chip.
  const check = shown.find((c) => c.key === "no_relationships")!;
  await panel.locator('[data-check="no_relationships"]').getByRole("link").click();
  await expect(page).toHaveURL(/\/cis\?quality=no_relationships$/);
  const chip = page.locator('[data-testid="filter-quality"]');
  await expect(chip).toContainText("Needs attention:");
  await expect(chip).toContainText("No relationships");
  await expect(page.locator(".page-header .count")).toHaveText(new RegExp(`^${check.count.toLocaleString("en-US")} (of [\\d,]+|total)$`));
  await page.reload();
  await expect(page.locator('[data-testid="filter-quality"]')).toBeVisible();
  await chip.getByRole("button", { name: "Remove the filter Needs attention: No relationships" }).click();
  await expect(page).not.toHaveURL(/quality=/);
  await expect(chip).toHaveCount(0);
});
