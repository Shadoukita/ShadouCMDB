import type { Page } from "@playwright/test";
import { classIdByName, createCi, expect, test } from "./support";

// The record's sections and save bar (SHAA-1670 design document, step 6b; audit R3, R4, R11): only a section
// the layout collapses has a disclosure toggle, read-only values and inputs share one label pattern, and the
// CI page, its edit page and the create page share the header and the save bar.
const stamp = Date.now().toString(36);
const NAME = `sections-${stamp}`;

const SECTION = "Network";

/** Serves the CI a layout of two sections: General, open, and Network, which the layout collapses. */
async function withCollapsedSection(page: Page) {
  await page.route("**/api/v1/configuration-items/*/layout", async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const res = await route.fetch();
    const body = await res.json();
    const field = (f: string) => ({ field: f, width: 1 });
    body.layout = {
      tabs: [
        {
          key: "general",
          label: "General",
          sections: [
            { key: "general", label: "General", columns: 3, width: 12, collapsed: false, fields: ["attributes.name", "attributes.status", "validUntil"].map(field) },
            { key: "network", label: SECTION, columns: 3, width: 12, collapsed: true, fields: ["attributes.hostname", "attributes.ip_address"].map(field) },
          ],
        },
      ],
      hiddenFields: [],
      readOnlyFields: [],
    };
    await route.fulfill({ response: res, json: body });
  });
}

test("only a collapsed section has a toggle; the save bar counts the changed fields", async ({ page, request }) => {
  const ci = await createCi(request, await classIdByName(request, "Server"), NAME);
  await withCollapsedSection(page);
  await page.goto(`/cis/${ci.id}`);

  const sections = page.locator(".layout-container .layout-panel");
  await expect(sections.first().locator(".panel-header h2")).toHaveText("General");
  // An open section is a plain panel: its heading is not a button.
  await expect(sections.first().locator(".panel-header button")).toHaveCount(0);
  await expect(page.locator(".layout-container details")).toHaveCount(0);

  const toggle = page.getByRole("button", { name: SECTION, exact: true });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  const body = page.locator(`#${await toggle.getAttribute("aria-controls")}`);
  await expect(body).toBeHidden();
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(body).toBeVisible();

  // Labels sit in secondary text above the value, read-only or not.
  const label = sections.first().locator(".field > label").first();
  const secondary = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--c-text-secondary").trim());
  expect(secondary).not.toBe("");
  const colour = await label.evaluate((el) => getComputedStyle(el).color);
  const expected = await page.evaluate((c) => {
    const probe = document.createElement("span");
    probe.style.color = c;
    document.body.append(probe);
    const out = getComputedStyle(probe).color;
    probe.remove();
    return out;
  }, secondary);
  expect(colour).toBe(expected);

  // The save bar: state on the left with the number of changed fields, Discard and Save on the right.
  await page.getByLabel("Valid until").fill("2099-01-01T00:00");
  const bar = page.getByRole("region", { name: "Unsaved changes" });
  await expect(bar).toContainText("Unsaved changes");
  await expect(bar).toContainText("1 field changed");
  await expect(bar.getByRole("button")).toHaveText(["Discard", "Save"]);
  await bar.getByRole("button", { name: "Discard" }).click();
  await expect(bar).toHaveCount(0);
});

test("the edit and create pages share the record header and the save bar", async ({ page, request }) => {
  await page.goto("/cis/new");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("New configuration item");
  await page.getByLabel("Class").selectOption({ label: "Server" });
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("New Server");
  await expect(page.getByRole("region", { name: "Save" }).getByRole("button", { name: "Create Server" })).toBeVisible();

  const ci = await createCi(request, await classIdByName(request, "Server"), `${NAME}-edit`);
  await page.goto(`/cis/${ci.id}/edit`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(`Edit ${NAME}-edit`);
  const meta = page.getByTestId("record-meta");
  await expect(meta.getByRole("link", { name: "Server" })).toBeVisible();
  await expect(meta).toContainText(`Version ${ci.version}`);
  const bar = page.getByRole("region", { name: "Save" });
  await expect(bar.getByRole("button", { name: "Save changes" })).toBeVisible();
  await expect(bar).not.toContainText("Unsaved changes");
  await page.getByLabel("Valid until").fill("2099-01-01T00:00");
  await expect(bar).toContainText("1 field changed");
  await bar.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${ci.id}$`));
});
