import { apiGet, apiSend, applySchemaChange, at, classIdByName, shownValue, snap, expect, test } from "./support";

// Administration › Data model › Dropdowns: a list that depends on a parent list
// (Model on Manufacturer), the attribute that names its parent field, and the
// cascading dropdowns on the CI form. Every name carries a stamp, so the walk can
// run against a shared (demo) database.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const MAKER = `E2E maker ${stamp}`;
const MODEL = `E2E model ${stamp}`;
const CLASS = `E2E switch ${stamp}`;
const CI = `e2e-switch-${stamp}`;
let classId = "";

interface Page_<T> {
  data: T[];
}
interface Attr {
  id: string;
  key: string;
  lookupListId: string | null;
  parentAttributeId: string | null;
}

const status = (page: import("@playwright/test").Page, text: string) => page.getByRole("status").filter({ hasText: text });

test("a list depends on a parent list, and each value names its parent value", async ({ page, request }) => {
  await page.goto("/admin/dropdowns");
  await expect(page.getByRole("heading", { level: 1, name: "Dropdowns" })).toBeVisible();

  // The parent list and its values.
  await page.getByRole("button", { name: "+ New list" }).first().click();
  await page.locator("#ll-name").fill(MAKER);
  await page.getByRole("button", { name: "Create list" }).click();
  await expect(status(page, `Created list ${MAKER}.`)).toBeVisible();
  for (const v of ["Cisco", "HPE"]) {
    await page.getByRole("button", { name: "+ Add value" }).first().click();
    await page.locator("#lookup-list-values-name").fill(v);
    await page.getByRole("button", { name: "Add value", exact: true }).click();
    await expect(status(page, `Added value ${v}.`)).toBeVisible();
  }

  // The child list names its parent list.
  await page.getByRole("button", { name: "+ New list" }).first().click();
  await page.locator("#ll-name").fill(MODEL);
  await page.locator("#ll-parentListId").selectOption({ label: MAKER });
  await page.getByRole("button", { name: "Create list" }).click();
  await expect(status(page, `Created list ${MODEL}.`)).toBeVisible();
  await expect(page.getByRole("row", { name: new RegExp(MODEL) }).getByRole("link", { name: MAKER })).toBeVisible();

  const values = page.getByRole("region", { name: `Values of “${MODEL}”` });
  await expect(values.getByLabel(`Belongs to (${MAKER})`)).toBeVisible();
  // A new value of a child list needs its parent value: the API's field error shows next to the field.
  await page.getByRole("button", { name: "+ Add value" }).first().click();
  await page.locator("#lookup-list-values-name").fill("Orphan");
  await page.getByRole("button", { name: "Add value", exact: true }).click();
  await expect(page.locator("#lookup-list-values-parentValueId-err")).toBeVisible();
  await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();

  for (const [v, maker] of [["Catalyst 9300", "Cisco"], ["ProLiant DL380", "HPE"], ["Nexus 9000", "Cisco"]]) {
    await page.getByRole("button", { name: "+ Add value" }).first().click();
    await page.locator("#lookup-list-values-name").fill(v);
    await page.locator("#lookup-list-values-parentValueId").selectOption({ label: maker });
    await page.getByRole("button", { name: "Add value", exact: true }).click();
    await expect(status(page, `Added value ${v}.`)).toBeVisible();
  }
  await expect(values.locator("tbody tr td:nth-child(2)")).toHaveText(["Catalyst 9300", "ProLiant DL380", "Nexus 9000"]);
  await expect(values.getByRole("columnheader", { name: `Belongs to (${MAKER})` })).toBeVisible();
  await expect(values.getByRole("row", { name: /ProLiant DL380/ })).toContainText("HPE");

  // Filtered by parent value (server-side, in the URL, so it survives a reload).
  await page.locator("#llv-parent").selectOption({ label: "Cisco" });
  await expect(page).toHaveURL(/[?&]parent=/);
  await page.reload();
  await expect(page.locator("#llv-parent").locator("option:checked")).toHaveText("Cisco");
  await expect(values.locator("tbody tr td:nth-child(2)")).toHaveText(["Catalyst 9300", "Nexus 9000"]);
  // Reordering within the filter keeps the positions among the other values.
  await page.getByRole("button", { name: "Move Nexus 9000 up" }).click();
  await expect(status(page, "Moved Nexus 9000.")).toBeVisible();
  await expect(values.locator("tbody tr td:nth-child(2)")).toHaveText(["Nexus 9000", "Catalyst 9300"]);
  // A value added while filtered starts with that parent value.
  await page.getByRole("button", { name: "+ Add value" }).first().click();
  await expect(page.locator("#lookup-list-values-parentValueId").locator("option:checked")).toHaveText("Cisco");
  await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();
  await snap(page, "35-dropdowns-dependent-list");

  const lists = await apiGet<Page_<{ id: string; name: string; parentListId: string | null }>>(request, `/lookup-lists?q=${encodeURIComponent(stamp)}`);
  const maker = lists.data.find((l) => l.name === MAKER)!;
  const model = lists.data.find((l) => l.name === MODEL)!;
  expect(model.parentListId).toBe(maker.id);
  const stored = await apiGet<Page_<{ name: string }>>(request, `/lookup-list-values?listId=${model.id}&sort=sortOrder`);
  expect(stored.data.map((v) => v.name)).toEqual(["Nexus 9000", "ProLiant DL380", "Catalyst 9300"]);

  await page.locator("#llv-parent").selectOption({ label: "All values" });
  await expect(page).not.toHaveURL(/[?&]parent=/);
  await expect(values.locator("tbody tr")).toHaveCount(3);
});

test("a lookup attribute on a child list names its parent field", async ({ page, request }) => {
  const created = await apiSend<{ id: string }>(request, "POST", "/ci-classes", {
    key: `e2e_switch_${stamp}`,
    name: CLASS,
    parentId: await classIdByName(request, "Hardware"),
  });
  classId = created.id;
  await page.goto(`/admin/classes/${classId}`);

  const add = async (label: string, list: string, parentField?: string) => {
    await page.getByRole("button", { name: "+ Add attribute" }).click();
    await page.locator("#ad-label").fill(label);
    await page.locator("#ad-type").selectOption({ label: "Lookup list" });
    await page.locator("#ad-list").selectOption({ label: list });
    if (parentField) {
      // The only candidate is chosen already.
      await expect(page.locator("#ad-parent-attr").locator("option:checked")).toHaveText(parentField);
    } else {
      await expect(page.locator("#ad-parent-attr")).toHaveCount(0);
    }
    await page.getByRole("button", { name: "Preview and add…" }).click();
    await applySchemaChange(page, "Add attribute", "ADD COLUMN");
    await expect(status(page, `Added attribute ${label}.`)).toBeVisible();
  };
  await add("Maker", MAKER);
  await add("Maker model", MODEL, "Maker");

  const attrs = await apiGet<Page_<Attr>>(request, `/ci-classes/${classId}/attributes`);
  const makerAttr = attrs.data.find((a) => a.key === "maker")!;
  expect(attrs.data.find((a) => a.key === "maker_model")?.parentAttributeId).toBe(makerAttr.id);
});

test("the CI form: the child dropdown follows its parent", async ({ page, request }) => {
  await page.goto(`/cis/new?classId=${classId}`);
  const maker = page.locator("#attr-maker");
  const model = page.locator("#attr-maker_model");
  const modelOptions = model.locator("option");

  // Empty and disabled, with a hint, until the parent is set.
  await expect(model).toBeDisabled();
  await expect(modelOptions).toHaveText(["Choose Maker first"]);
  await expect(page.locator("#attr-maker_model-hint")).toContainText("depends on Maker");

  // Manufacturer = Cisco → Model shows only Cisco models.
  await maker.selectOption({ label: "Cisco" });
  await expect(model).toBeEnabled();
  await expect(modelOptions).toHaveText(["Not set", "Nexus 9000", "Catalyst 9300"]);
  await model.selectOption({ label: "Catalyst 9300" });

  // Another parent clears a child value it does not offer.
  await maker.selectOption({ label: "HPE" });
  await expect(modelOptions).toHaveText(["Not set", "ProLiant DL380"]);
  await expect(model).toHaveValue("");
  // Clearing the parent disables the child again.
  await maker.selectOption({ label: "Not set" });
  await expect(model).toBeDisabled();

  await maker.selectOption({ label: "Cisco" });
  await model.selectOption({ label: "Nexus 9000" });
  await page.locator("#attr-name").fill(CI);
  await page.locator("#attr-status").selectOption({ label: "In service" });
  await snap(page, "36-ci-form-cascading-dropdown");
  await page.getByRole("button", { name: `Create ${CLASS}` }).click();
  await expect(page).toHaveURL(/\/cis\/[0-9a-f-]{36}$/);
  await expect.poll(() => shownValue(page, "Maker model")).toBe("Nexus 9000");

  const ciId = page.url().split("/").pop()!;
  const ci = await apiGet<{ attributes: Record<string, unknown> }>(request, `/configuration-items/${ciId}`);
  const makerValues = await apiGet<Page_<{ id: string; name: string }>>(
    request,
    `/lookup-list-values?listId=${(await apiGet<Page_<Attr>>(request, `/ci-classes/${classId}/attributes`)).data.find((a) => a.key === "maker")!.lookupListId}`,
  );
  expect(ci.attributes.maker).toBe(makerValues.data.find((v) => v.name === "Cisco")!.id);

  // Editing shows the stored value; choosing another parent clears it.
  await page.goto(`/cis/${ciId}/edit`);
  await expect(page.locator("#attr-maker_model").locator("option:checked")).toHaveText("Nexus 9000");
  await page.locator("#attr-maker").selectOption({ label: "HPE" });
  await expect(page.locator("#attr-maker_model")).toHaveValue("");
  await page.locator("#attr-maker_model").selectOption({ label: "ProLiant DL380" });
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page).toHaveURL(at(`/cis/${ciId}`));
  await expect.poll(() => shownValue(page, "Maker model")).toBe("ProLiant DL380");
});

test("the row actions fit at a 1280 px viewport, however long the description (GH#110)", async ({ page, request }) => {
  const name = `E2E wide ${stamp}`;
  await apiSend(request, "POST", "/lookup-lists", {
    key: `e2e_wide_${stamp}`,
    name,
    description: "A long description that an administrator wrote to explain when operators should choose which value. ".repeat(4),
  });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto("/admin/dropdowns");
  const rows = page.getByRole("region", { name: "Lookup lists" }).locator("tbody tr");
  await expect(rows.filter({ hasText: name })).toHaveCount(1);
  for (const row of await rows.all()) {
    const cell = (await row.locator("td.row-actions").boundingBox())!;
    for (const button of await row.locator("td.row-actions > button").all()) {
      await expect(button).toBeVisible();
      const b = (await button.boundingBox())!;
      // Inside its cell (which hides overflow) and inside the viewport, without scrolling sideways.
      expect(b.x + b.width).toBeLessThanOrEqual(cell.x + cell.width + 0.5);
      expect(b.x + b.width).toBeLessThanOrEqual(1280);
    }
  }
  await snap(page, "dropdowns-1280");
});
