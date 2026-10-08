import { apiGet, checkA11y, chooseTheme, classIdByName, expect, test } from "./support";

// The CI detail in the reference-mockup look (design document §0, step 12d): the page head with the class
// tile, the name in the data font, the class/state chips and "ident · Updated … by …", tabs with counts,
// and the built-in arrangement as the default layout: the field sections in one card beside the
// relationships (grouped, with a direction filter and type pills) and the newest history as a timeline.
// Read-only on a demo-seed Server: creating a Server here would change which one other specs open first.

async function firstServer(request: Parameters<typeof classIdByName>[0]) {
  const serverId = await classIdByName(request, "Server");
  const ci = (await apiGet<{ data: { id: string; label: string; ident: string }[] }>(request, `/configuration-items?classId=${serverId}&sort=label&limit=1`)).data[0];
  const rels = await apiGet<{ page: { total: number } }>(request, `/relationships?ciId=${ci.id}&limit=1`);
  const history = await apiGet<{ page: { total: number } }>(request, `/audit-log?entityId=${ci.id}&limit=1`);
  return { serverId, ci, relTotal: rels.page.total, historyTotal: history.page.total };
}

test("CI detail: page head, tab counts, grouped relationships and the history timeline", async ({ page, request }, testInfo) => {
  const { serverId, ci, relTotal, historyTotal } = await firstServer(request);
  await page.goto(`/cis/${ci.id}`);

  // The page head: the name in the data font, the class chip to its inventory, the ident and the last update.
  const h1 = page.getByRole("heading", { level: 1 });
  await expect(h1).toHaveText(ci.label);
  await expect(h1).toHaveClass(/\bmono\b/);
  const meta = page.getByTestId("record-meta");
  await expect(meta.getByRole("link", { name: "Server", exact: true })).toHaveAttribute("href", `/cis?classId=${serverId}`);
  await expect(meta).toContainText(ci.ident);
  // Who made the newest change comes from the history (gap G16).
  await expect(meta.locator("time")).toHaveText(/^Updated .+ by .+$/);

  // Tabs with counts: the count is in the accessible name, not in the tab's text.
  const tabs = page.getByRole("tablist", { name: "CI sections" }).getByRole("tab");
  await expect(tabs).toHaveText(["Overview", "Relationship map", "Impact", "Notes", "History"]);
  await expect(page.getByRole("tab", { name: `Relationship map ${relTotal}`, exact: true })).toBeVisible();
  await expect(page.getByRole("tab", { name: `History ${historyTotal}`, exact: true })).toBeVisible();

  // The default layout: the sections in one card, under overline headings.
  const card = page.locator(".record-overview .lp-stacked");
  await expect(card.locator(".layout-panel > .panel-header h2").first()).toHaveText("General");
  await expect(card.locator(".layout-panel > .panel-header h2").last()).toHaveText("Record");

  // Relationships: grouped by how the edge reads, each row a link with a type pill and its direction.
  const rel = page.getByRole("region", { name: /^Relationships/ });
  await expect(rel.getByRole("listitem")).toHaveCount(relTotal);
  const located = rel.getByRole("list", { name: /^is located in/ }).getByRole("listitem");
  await expect(located.getByRole("link")).toHaveText("FRA1 Rack A01");
  await expect(located.locator(".rel-type")).toContainText("located_in");
  await expect(located.getByRole("img", { name: "Outgoing" })).toBeVisible();
  await expect(located.getByRole("button", { name: "Actions for FRA1 Rack A01" })).toBeVisible();
  await rel.getByRole("searchbox", { name: "Filter relationships" }).fill("no such relationship");
  await expect(rel.getByRole("status")).toHaveText("No relationship matches the filter.");
  await rel.getByRole("searchbox", { name: "Filter relationships" }).fill("");

  // The newest history entries as a timeline; Full history opens the History tab.
  const history = page.getByRole("region", { name: /^History/ });
  await expect(history.locator("ol.event-timeline > li").first()).toBeVisible();
  await expect(history.locator("ol.event-timeline > li")).toHaveCount(Math.min(historyTotal, 5));

  await checkA11y(page, testInfo, "ci-detail-light");
  await chooseTheme(page, "dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await checkA11y(page, testInfo, "ci-detail-dark");
  await chooseTheme(page, "");

  await history.getByRole("button", { name: "Full history" }).click();
  await expect(page.getByRole("tab", { name: /^History/ })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("group", { name: "Filter by source" })).toBeVisible();
  await expect(page.locator("ol.event-timeline > li").first().locator(".event-time")).toHaveText(/^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d$/);
});

test("CI detail: the new texts come from the German catalog", async ({ page, request }) => {
  const { ci } = await firstServer(request);
  await page.addInitScript(() => ((window as unknown as { __shadoucmdbTestLocale: string }).__shadoucmdbTestLocale = "de"));
  await page.goto(`/cis/${ci.id}`);
  await expect(page.getByRole("tablist", { name: "CI-Bereiche" }).getByRole("tab")).toHaveText(["Übersicht", "Beziehungskarte", "Auswirkung", "Notizen", "Verlauf"]);
  await expect(page.getByTestId("record-meta").locator("time")).toHaveText(/^Geändert .+ von .+$/);
  const rel = page.getByRole("region", { name: /^Beziehungen/ });
  await expect(rel.getByRole("radiogroup", { name: "Richtung" }).getByRole("radio", { name: "Alle" })).toBeChecked();
  await expect(rel.getByRole("searchbox", { name: "Beziehungen filtern" })).toBeVisible();
  await expect(page.getByRole("region", { name: /^Verlauf/ }).getByRole("button", { name: "Gesamter Verlauf" })).toBeVisible();
});
