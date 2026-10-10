// Unit tests for the message catalog (SHAA-927 §5.9, §7.4 item 13). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import { currentLocale, formatNumber, hasMessage, parseMessage, setLocaleForTests, t, tAround } from "../src/i18n/index";
import { formatRelative } from "../src/lib/format";
import { BUILTIN, fieldLabel, pageLabel, SORT_FIELDS, widgetLabel } from "../src/lib/uiSettings";
import { de } from "../src/i18n/de";
import { en } from "../src/i18n/en";

afterEach(() => setLocaleForTests(null));

describe("catalogs", () => {
  test("de has exactly the keys of en", () => {
    assert.deepEqual(Object.keys(de).sort(), Object.keys(en).sort());
  });
  test("every message parses in both languages and uses the same parameters", () => {
    type Tree = ReturnType<typeof parseMessage>;
    const walk = (nodes: Tree, out: Set<string>): Set<string> => {
      for (const node of nodes) {
        if (typeof node === "string" || node.kind === "hash") continue;
        out.add(node.name);
        if (node.kind === "plural") for (const branch of node.branches.values()) walk(branch, out);
      }
      return out;
    };
    const names = (src: string) => [...walk(parseMessage(src), new Set())].sort();
    for (const key of Object.keys(en) as (keyof typeof en)[]) {
      assert.doesNotThrow(() => parseMessage(en[key]), key);
      assert.doesNotThrow(() => parseMessage(de[key]), key);
      assert.deepEqual(names(de[key]), names(en[key]), key);
    }
  });
  test("malformed messages are rejected", () => {
    assert.throws(() => parseMessage("{n, plural, one {# x}}"), /other/);
    assert.throws(() => parseMessage("{n, select, a {x} other {y}}"), /unsupported/);
    assert.throws(() => parseMessage("Open {name"), /unexpected end/);
    assert.throws(() => parseMessage("stray }"), /unmatched/);
  });
});

describe("locale", () => {
  test("the UI is English by default, with no setting to change it", () => {
    assert.equal(currentLocale(), "en");
    assert.equal(t("services.nav"), "Business services");
  });
  test("the test-only override forces German", () => {
    setLocaleForTests("de");
    assert.equal(t("services.nav"), "Business-Services");
    setLocaleForTests(null);
    assert.equal(t("services.nav"), "Business services");
  });
  test("the Playwright hook on globalThis forces German", () => {
    const g = globalThis as { __shadoucmdbTestLocale?: string };
    g.__shadoucmdbTestLocale = "de";
    try {
      assert.equal(t("common.retry"), "Erneut versuchen");
      g.__shadoucmdbTestLocale = "fr";
      assert.equal(t("common.retry"), "Retry");
    } finally {
      delete g.__shadoucmdbTestLocale;
    }
  });
});

describe("parameters and plurals", () => {
  test("simple parameters", () => {
    assert.equal(t("services.owners.remove", { name: "Ops team", role: "Technical owners" }), "Remove Ops team as Technical owners");
    assert.equal(t("services.picker.selected", { n: 0 }), "Selected (0)");
  });
  test("English plurals", () => {
    assert.equal(t("services.picker.submit", { n: 1 }), "Add 1 member");
    assert.equal(t("services.picker.submit", { n: 3 }), "Add 3 members");
    assert.equal(t("services.picker.submit", { n: 0 }), "Add 0 members");
    assert.equal(t("services.picker.submit", { n: 1200 }), "Add 1,200 members");
    assert.equal(
      t("services.members.removeConfirm", { n: 2, service: "Payroll" }),
      "Remove 2 members from Payroll? The CIs themselves are not deleted.",
    );
    assert.equal(
      t("services.delete.nested", { n: 1 }),
      "It is also part of 1 other business service; it is removed from it.",
    );
    assert.equal(
      t("services.delete.nested", { n: 4 }),
      "It is also part of 4 other business services; it is removed from them.",
    );
  });
  test("German plurals", () => {
    setLocaleForTests("de");
    assert.equal(t("services.picker.submit", { n: 1 }), "1 Mitglied hinzufügen");
    assert.equal(t("services.picker.submit", { n: 5 }), "5 Mitglieder hinzufügen");
    assert.equal(t("services.picker.added", { n: 1200 }), "1.200 Mitglieder hinzugefügt.");
    assert.equal(
      t("services.delete.body", { n: 1 }),
      "Seine 1 Mitgliedschaft wird entfernt. Die Mitglieds-CIs selbst werden nicht gelöscht.",
    );
    assert.equal(
      t("groups.delete.body", { n: 2 }),
      "Sie ist für 2 Business-Services verantwortlich und wird dort überall als Verantwortliche entfernt.",
    );
  });
  test("a missing parameter stays visible instead of rendering undefined", () => {
    assert.equal(t("services.picker.title"), "Add members to {service}");
    assert.equal(t("services.owners.remove", { name: "Ops team" }), "Remove Ops team as {role}");
    assert.equal(t("services.partOf.via", { service: null }), "via {service}");
  });
  test("a missing or non-numeric plural count falls back to `other` and keeps the placeholder", () => {
    assert.equal(t("services.picker.submit"), "Add {n} members");
    assert.equal(t("services.picker.submit", { n: "three" }), "Add {n} members");
    assert.equal(
      t("services.members.removeConfirm", { service: "Payroll" }),
      "Remove {n} members from Payroll? The CIs themselves are not deleted.",
    );
  });
});

describe("text around markup", () => {
  test("tAround splits a message at one parameter, in the translator's word order", () => {
    assert.deepEqual(tAround("auth.signIn.lostAdmin", "command"), ["Lost access to every administrator account? Run ", " on the server."]);
    setLocaleForTests("de");
    assert.deepEqual(tAround("auth.signIn.lostAdmin", "command"), [
      "Kein Zugriff mehr auf ein Administratorkonto? Führen Sie ",
      " auf dem Server aus.",
    ]);
  });
  test("the recovery-code count is a plural in both languages", () => {
    assert.equal(t("account.mfa.regenerateBody", { n: 1 }), "You get 10 new codes; the 1 you have now stops working.");
    assert.equal(t("account.mfa.regenerateBody", { n: 7 }), "You get 10 new codes; the 7 you have now stop working.");
    setLocaleForTests("de");
    assert.equal(t("account.mfa.regenerateBody", { n: 1 }), "Sie erhalten 10 neue Codes; der 1 Code, den Sie jetzt haben, wird ungültig.");
    assert.equal(t("account.mfa.regenerateBody", { n: 7 }), "Sie erhalten 10 neue Codes; die 7 Codes, die Sie jetzt haben, werden ungültig.");
  });
});

describe("missing messages", () => {
  test("a message missing from the German catalog falls back to English, never to its key", () => {
    // A key no other test renders, so no parsed German message is cached for it.
    const saved = de["dataModel.empty.createClass"];
    delete (de as Partial<typeof de>)["dataModel.empty.createClass"];
    try {
      setLocaleForTests("de");
      assert.equal(t("dataModel.empty.createClass"), "Create a class");
    } finally {
      de["dataModel.empty.createClass"] = saved;
    }
  });
  test("keys built at run time are checked before use", () => {
    assert.equal(hasMessage("nav.page.dashboard"), true);
    assert.equal(hasMessage("nav.page.reports"), false);
    assert.equal(hasMessage("toString"), false);
    assert.equal(pageLabel("reports" as Parameters<typeof pageLabel>[0]), "reports");
  });
});

describe("app shell and dashboard", () => {
  test("page and widget names come from the catalog", () => {
    assert.equal(pageLabel("inventory"), "All configuration items");
    assert.equal(widgetLabel("count_by_class"), "CIs by class");
    setLocaleForTests("de");
    assert.equal(pageLabel("inventory"), "Alle CIs");
    assert.equal(widgetLabel("saved_search"), "Gespeicherte Suche");
  });
  test("built-in field names come from the catalog, for columns, forms and sort choices (SHAA-2406)", () => {
    assert.equal(fieldLabel("updatedAt", []), "Updated");
    assert.equal(BUILTIN.get("validFrom")?.label, "Valid from");
    setLocaleForTests("de");
    assert.equal(fieldLabel("label", []), "Bezeichnung");
    assert.equal(fieldLabel("updatedAt", []), "Geändert");
    assert.equal(BUILTIN.get("criticality")?.label, "Kritikalität");
    assert.equal(SORT_FIELDS.find((s) => s.field === "createdAt")?.label, "Angelegt");
    assert.equal(fieldLabel("attributes.nope", []), "nope"); // not a built-in field: unchanged
  });
  test("numbers and relative times follow the locale", () => {
    const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();
    assert.equal(formatNumber(1200), "1,200");
    assert.equal(formatRelative(ago(5)), "5 min ago");
    assert.equal(formatRelative(ago(3 * 1440)), "3 d ago");
    setLocaleForTests("de");
    assert.equal(formatNumber(1200), "1.200");
    assert.equal(formatRelative(ago(0)), "gerade eben");
    assert.equal(formatRelative(ago(5)), "vor 5 Min.");
    assert.equal(formatRelative(ago(1440)), "vor 1 Tag");
    assert.equal(formatRelative(ago(3 * 1440)), "vor 3 Tagen");
    assert.equal(t("globalSearch.seeAll", { n: 1200 }), "Alle 1.200 Treffer anzeigen ↵");
  });
});
