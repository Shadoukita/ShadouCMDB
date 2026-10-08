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
