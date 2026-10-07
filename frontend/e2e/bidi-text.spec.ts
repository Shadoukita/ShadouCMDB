import { apiGet, apiSend, classIdByName, createCi, snap, expect, test } from "./support";

// GH#289: bidi controls are allowed only in multiline fields (notes, descriptions), so the UI shows
// user text in its own bidi context: an override in a note must not reorder the text around it. And
// admin dialogs send only the fields that changed, so a row whose stored name holds a character the
// API now refuses (older API calls or imports) can still have its description edited.
test.describe.configure({ mode: "serial" });

const stamp = Date.now().toString(36);
const RLO = "‮";

interface Ci {
  id: string;
  version: number;
}

test("an override in a note stays inside the note on the detail page and in the history", async ({ page, request }) => {
  const ci = await createCi(request, await classIdByName(request, "Application"), `e2e-bidi-${stamp}`, { notes: `${RLO}old note` });
  const { version } = await apiGet<Ci>(request, `/configuration-items/${ci.id}`);
  await apiSend(request, "PATCH", `/configuration-items/${ci.id}`, { version, attributes: { notes: "new note" } });

  await page.goto(`/cis/${ci.id}`);
  await page.getByRole("tab", { name: "History" }).click();
  const line = page.locator("ul.diff li", { has: page.locator("code", { hasText: /notes$/ }) });
  const [from, to] = [line.locator("del"), line.locator("ins")];
  await expect(from).toHaveText(`${RLO}old note`);
  await expect(to).toHaveText("new note");
  // Unisolated, the override would run on to the end of the line and put the new value first.
  const [a, b] = [await from.boundingBox(), await to.boundingBox()];
  expect(b!.x).toBeGreaterThan(a!.x + a!.width);
  await snap(page, "bidi-history");
});

test("editing a list's description sends only the description, not its stored name", async ({ page, request }) => {
  const list = await apiSend<{ id: string; name: string }>(request, "POST", "/lookup-lists", { key: `e2e_bidi_${stamp}`, name: `E2E bidi ${stamp}` });
  // A name stored before GH#289 (the API refuses it now): served as such to the page.
  const legacy = `E2E bidi${RLO} ${stamp}`;
  await page.route(/\/api\/v1\/lookup-lists(\?|$)/, async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const res = await route.fetch();
    const body = (await res.json()) as { data: { id: string; name: string }[] };
    for (const l of body.data) if (l.id === list.id) l.name = legacy;
    await route.fulfill({ response: res, json: body });
  });
  const sent: unknown[] = [];
  page.on("request", (r) => r.method() === "PATCH" && r.url().endsWith(`/lookup-lists/${list.id}`) && sent.push(r.postDataJSON()));

  await page.goto("/admin/dropdowns");
  await page.getByRole("button", { name: `Actions for ${legacy}` }).click();
  await page.getByRole("menuitem", { name: "Edit" }).click();
  await page.locator("#ll-description").fill("Kept for the 2026 audit");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved list" })).toBeVisible();
  expect(sent).toEqual([{ description: "Kept for the 2026 audit" }]);

  await snap(page, "bidi-dialog-saved");
});
