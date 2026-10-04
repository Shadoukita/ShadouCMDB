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

test("inventory: the query bar's colouring lies exactly over the text, also once it scrolls", async ({ page, request }) => {
  // Two real class keys, so the first error is the negation and not an unknown class.
  const classes = await apiGet<{ data: { key: string }[] }>(request, "/ci-classes?limit=200");
  expect(classes.data.length, "two classes").toBeGreaterThanOrEqual(2);
  const [a, b] = classes.data;

  await page.goto("/cis");
  const bar = page.locator("#f-q");
  const overlay = page.locator(".query-overlay");
  await expect(overlay).toHaveAttribute("aria-hidden", "true");

  // Long enough to scroll the input sideways, with every kind of run and an error.
  const text = `class:${a.key},${b.key} -deleted:include nosuchfilter:me "quoted text" ${"fra1-esx-01 ".repeat(20).trim()}`;
  await bar.click();
  await bar.fill(text);
  await bar.press("End");
  await expect(bar).toHaveAttribute("aria-invalid", "true");
  await expect(overlay.locator(".qs-key").first()).toHaveText("class");
  await expect(overlay.locator(".qs-negation")).toHaveText("-");
  await expect(overlay.locator(".qs-error").first()).toHaveText("-");

  const m = await page.evaluate(() => {
    const input = document.querySelector<HTMLInputElement>("#f-q")!;
    const over = document.querySelector<HTMLElement>(".query-overlay")!;
    const props = ["font-family", "font-size", "font-weight", "font-style", "letter-spacing", "word-spacing", "font-kerning", "font-variant-ligatures", "padding-left", "border-left-width", "text-indent"];
    const style = (el: Element) => Object.fromEntries(props.map((p) => [p, getComputedStyle(el).getPropertyValue(p)]));
    const box = (el: Element) => {
      const r = el.getBoundingClientRect();
      return [r.left, r.top, r.width, r.height].map(Math.round);
    };
    const first = over.querySelector("span")!.getBoundingClientRect();
    const cs = getComputedStyle(over);
    return {
      inputStyle: style(input),
      overlayStyle: style(over),
      inputBox: box(input),
      overlayBox: box(over),
      text: over.textContent,
      value: input.value,
      inputScroll: input.scrollLeft,
      overlayScroll: over.scrollLeft,
      // Where the first glyph starts, against the content edge both share.
      firstLeft: first.left + over.scrollLeft - over.getBoundingClientRect().left,
      contentLeft: parseFloat(cs.borderLeftWidth) + parseFloat(cs.paddingLeft),
      // The glyphs sit on the input's vertical centre.
      firstMid: first.top + first.height / 2,
      inputMid: input.getBoundingClientRect().top + input.getBoundingClientRect().height / 2,
    };
  });
  expect(m.overlayStyle).toEqual(m.inputStyle);
  expect(m.overlayBox).toEqual(m.inputBox);
  expect(m.text).toBe(m.value);
  expect(m.inputScroll).toBeGreaterThan(0);
  expect(Math.abs(m.overlayScroll - m.inputScroll)).toBeLessThanOrEqual(1);
  expect(Math.abs(m.firstLeft - m.contentLeft)).toBeLessThanOrEqual(0.5);
  expect(Math.abs(m.firstMid - m.inputMid)).toBeLessThanOrEqual(2);

  // Back to the start, the overlay follows.
  await bar.press("Home");
  await expect.poll(() => overlay.evaluate((el) => el.scrollLeft)).toBe(0);
});
