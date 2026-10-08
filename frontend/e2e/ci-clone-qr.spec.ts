import { apiGet, apiSend, ciIdByName, classIdByName, createCi, expect, test } from "./support";

// Clone (gap G11) and the QR label (gap G12) on the CI page (SHAA-2358). Both are built in the browser: a clone is
// the create form prefilled from the CI and saved through POST /configuration-items; the QR code is drawn locally.
// The CIs are named "zz-…" so they never come first in a list another spec opens.

interface Ci {
  id: string;
  ident: string;
  label: string;
  classId: string;
  attributes: Record<string, unknown>;
  criticality: { id: string } | null;
}

test("clone: the form starts from the CI's values, saves as a new CI and leaves the source alone", async ({ page, request }) => {
  const stamp = Date.now();
  const serverId = await classIdByName(request, "Server");
  const created = await createCi(request, serverId, `zz-clone-src-${stamp}`, { hostname: `zz-src-${stamp}`, ip_address: "10.99.0.7", cpu_cores: 24 });
  const rack = await ciIdByName(request, "FRA1 Rack A01");
  const types = await apiGet<{ data: { id: string; key: string }[] }>(request, "/relationship-types?limit=200");
  const located = types.data.find((x) => x.key === "located_in")!;
  await apiSend(request, "POST", "/relationships", { relationshipTypeId: located.id, sourceCiId: created.id, targetCiId: rack });
  const before = await apiGet<Ci>(request, `/configuration-items/${created.id}`);

  await page.goto(`/cis/${created.id}`);
  await page.getByTestId("ci-clone").click();
  await expect(page).toHaveURL(new RegExp(`/cis/new\\?classId=${serverId}&cloneFrom=${created.id}$`));

  // The notice names the source and what is left empty; the form holds the copied values, not the name or the IP.
  const notice = page.getByTestId("clone-notice");
  await expect(notice.getByRole("link", { name: created.label })).toHaveAttribute("href", `/cis/${created.id}`);
  await expect(notice).toContainText("Not copied: the ident, relationships");
  await expect(notice).toContainText("Left empty for you to fill in: Name");
  await expect(notice).toContainText("IP address");
  await expect(page.locator("#attr-name")).toHaveValue("");
  await expect(page.locator("#attr-ip_address")).toHaveValue("");
  await expect(page.locator("#attr-hostname")).toHaveValue(`zz-src-${stamp}`);
  await expect(page.locator("#attr-cpu_cores")).toHaveValue("24");
  await expect(page.locator("#attr-status")).toHaveValue(String(before.attributes.status));

  // The name is required: saving without one is caught on the form.
  await page.getByRole("button", { name: "Create Server" }).click();
  await expect(page.locator("#attr-name")).toBeFocused();
  await page.locator("#attr-name").fill(`zz-clone-copy-${stamp}`);
  await page.locator("#attr-hostname").fill(`zz-copy-${stamp}`);
  await page.getByRole("button", { name: "Create Server" }).click();

  await expect(page.getByRole("heading", { level: 1 })).toHaveText(`zz-clone-copy-${stamp}`);
  const copyId = /\/cis\/([0-9a-f-]{36})$/.exec(page.url())?.[1];
  expect(copyId).toBeTruthy();
  const copy = await apiGet<Ci>(request, `/configuration-items/${copyId}`);
  expect(copy.id).not.toBe(created.id);
  expect(copy.ident).not.toBe(created.ident);
  expect(copy.classId).toBe(serverId);
  expect(copy.attributes.cpu_cores).toBe(24);
  expect(copy.attributes.status).toBe(before.attributes.status);
  expect(copy.attributes.ip_address ?? null).toBeNull();
  // No relationships were copied; the source keeps its own, its values and its version.
  expect((await apiGet<{ page: { total: number } }>(request, `/relationships?ciId=${copyId}&limit=1`)).page.total).toBe(0);
  expect((await apiGet<{ page: { total: number } }>(request, `/relationships?ciId=${created.id}&limit=1`)).page.total).toBe(1);
  const after = await apiGet<Ci & { version: number }>(request, `/configuration-items/${created.id}`);
  expect(after.attributes).toEqual(before.attributes);
  expect(after.version).toBe(created.version);
  // The new CI is audited as a create, like any other.
  const audit = await apiGet<{ data: { action: string }[] }>(request, `/audit-log?entityId=${copyId}&limit=5`);
  expect(audit.data.map((e) => e.action)).toContain("create");

  // Cancel on a clone goes back to the source.
  await page.goto(`/cis/new?classId=${serverId}&cloneFrom=${created.id}`);
  await page.getByRole("link", { name: "Cancel" }).click();
  await expect(page).toHaveURL(new RegExp(`/cis/${created.id}$`));
});

test("clone: a source that does not exist says so instead of a blank form", async ({ page, request }) => {
  const serverId = await classIdByName(request, "Server");
  await page.goto(`/cis/new?classId=${serverId}&cloneFrom=00000000-0000-4000-8000-000000000000`);
  await expect(page.getByRole("alert")).toContainText("Could not load the CI to clone");
});

test("QR label: the code of the CI's permalink, downloads as SVG and PNG", async ({ page, request }) => {
  const stamp = Date.now();
  const serverId = await classIdByName(request, "Server");
  const created = await createCi(request, serverId, `zz-qr-${stamp}`);

  await page.goto(`/cis/${created.id}`);
  await page.getByTestId("ci-qr").click();
  const dialog = page.getByRole("dialog", { name: "QR label" });
  await expect(dialog).toBeVisible();
  const origin = new URL(page.url()).origin;
  await expect(dialog.getByTestId("qr-permalink")).toHaveText(`${origin}/cis/${created.id}`);
  await expect(dialog.getByRole("img", { name: `QR code linking to zz-qr-${stamp}` })).toBeVisible();
  await expect(dialog.getByTestId("qr-label")).toContainText(created.ident);

  const svg = page.waitForEvent("download");
  await dialog.getByRole("button", { name: "Download SVG" }).click();
  const svgFile = await svg;
  expect(svgFile.suggestedFilename()).toBe(`${created.ident}-qr.svg`);
  const svgText = await new Response((await svgFile.createReadStream()) as unknown as ReadableStream).text();
  expect(svgText).toContain("<svg");
  expect(svgText).toContain(`zz-qr-${stamp}`);

  const png = page.waitForEvent("download");
  await dialog.getByRole("button", { name: "Download PNG" }).click();
  const pngFile = await png;
  expect(pngFile.suggestedFilename()).toBe(`${created.ident}-qr.png`);

  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(dialog).toBeHidden();
});
