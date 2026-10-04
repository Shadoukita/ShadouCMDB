import { apiGet, expect, test } from "./support";

// The inventory's query bar: key:value tokens are a front end to the list's URL filters
// (lib/queryBar), with key and value suggestions from the data model.

test("inventory: the query bar writes key:value tokens into the URL filters, and back", async ({ page, request }) => {
  const classes = await apiGet<{ data: { id: string; key: string; name: string }[] }>(request, "/ci-classes?limit=200");
  const server = classes.data.find((c) => c.name === "Server");
  expect(server, "class Server").toBeTruthy();

  await page.goto("/cis");
  const bar = page.locator("#f-q");
  await expect(bar).toHaveAttribute("role", "combobox");

  // A begun word offers the keys; a key offers its values, by key and name.
  await bar.click();
  await bar.pressSequentially("cla");
  const list = page.getByRole("listbox", { name: "Filter suggestions" });
  await expect(list.getByRole("option", { name: /^class:/ })).toBeVisible();
  await bar.press("ArrowDown");
  await bar.press("Enter");
  await expect(bar).toHaveValue("class:");
  // Part of the key: a value typed in full has nothing left to offer.
  await bar.pressSequentially(server!.key.slice(0, -1));
  await list.getByRole("option").filter({ has: page.locator(".query-suggestion-detail", { hasText: /^Server$/ }) }).click();
  await expect(bar).toHaveValue(`class:${server!.key} `);
  await expect(page).toHaveURL(new RegExp(`classId=${server!.id}`));
  // The class select follows, as any control on the same URL state does.
  await expect(page.locator("#f-class")).toHaveValue(server!.id);

  // A key the API cannot filter on is reported under the bar, and the list keeps its filters.
  // Lookup lists are keys too, and other specs create them (an "owner" list among them),
  // so the key is checked against the lists first.
  const lists = await apiGet<{ data: { key: string }[] }>(request, "/lookup-lists?limit=200");
  expect(lists.data.map((l) => l.key.toLowerCase())).not.toContain("nosuchfilter");
  await bar.fill(`class:${server!.key} nosuchfilter:me`);
  await expect(page.getByText("“nosuchfilter” is not a filter.")).toBeVisible();
  await expect(bar).toHaveAttribute("aria-invalid", "true");
  await expect(page).toHaveURL(new RegExp(`classId=${server!.id}`));

  // Excluding has no API filter either.
  await bar.fill(`-class:${server!.key}`);
  await expect(page.getByText("excluding values is not supported")).toBeVisible();

  // Removing the token removes the filter.
  await bar.fill("");
  await bar.press("Enter");
  await expect(page).not.toHaveURL(/classId=/);
  await expect(page.locator("#f-class")).toHaveValue("");

  // And the other way: a select writes its token into the bar.
  await page.locator("#f-deleted").selectOption("include");
  await expect(bar).toHaveValue("deleted:include");
});
