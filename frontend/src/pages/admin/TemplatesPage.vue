<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useInstallTemplate, useTemplates } from "../../api/datamodel";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import type { SchemaChange } from "../../api/schemaChanges";
import LoadingState from "../../components/LoadingState.vue";
import { t, type MessageKey } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";

/**
 * Administration › Data model › Starter templates. A fresh install has no classes or lookups; a starter
 * template adds a ready-made model in one click. Installing is idempotent: rows that already exist (matched
 * by key) are left alone. The page has the CI page's head band without tabs (design step 9c-4), each
 * template is a panel with its status as a pill, and no style attribute is left (audit X16).
 */
useDocumentTitle(() => t("admin.section.templates"));
const templates = useTemplates();
const install = useInstallTemplate();
const results = ref<Record<string, InstallResult>>({});
const installing = ref<string | null>(null);

type Part = "areas" | "classes" | "attributeDefinitions" | "relationshipTypes" | "relationshipRules" | "lookupLists" | "lookupListValues";
type Counts = Record<Part, number>;
type Status = "not_installed" | "partial" | "installed";
/** The fields of a template (and an install result) this page reads. */
interface Template {
  key: string;
  name: string;
  status: Status;
}
interface InstallResult {
  created: Counts;
  existing: Counts;
  skipped: string[];
  schemaChange: SchemaChange | null;
}
const PARTS: Part[] = ["areas", "classes", "attributeDefinitions", "relationshipTypes", "relationshipRules", "lookupLists", "lookupListValues"];
const PART_LABEL: Record<Part, MessageKey> = {
  areas: "templates.part.areas",
  classes: "templates.part.classes",
  attributeDefinitions: "templates.part.attributeDefinitions",
  relationshipTypes: "templates.part.relationshipTypes",
  relationshipRules: "templates.part.relationshipRules",
  lookupLists: "templates.part.lookupLists",
  lookupListValues: "templates.part.lookupListValues",
};
const STATUS_TONE: Record<Status, string> = { not_installed: "off", partial: "warn", installed: "ok" };
const STATUS_LABEL: Record<Status, MessageKey> = {
  not_installed: "templates.status.not_installed",
  partial: "templates.status.partial",
  installed: "templates.status.installed",
};

/** Nothing of any template is present yet: the empty-install state. */
const emptyInstall = computed(() => (templates.data.value ?? []).every((tpl) => PARTS.every((p) => tpl.present[p] === 0)));
const installedCount = computed(() => (templates.data.value ?? []).filter((tpl) => tpl.status === "installed").length);
/** The empty-state text around its two links, in the catalog's order. */
const emptyBody = computed(() => t("templates.empty.body", { classes: "\u0000", dropdowns: "\u0000" }).split("\u0000"));

async function run(tpl: Template) {
  installing.value = tpl.key;
  install.reset();
  try {
    results.value = { ...results.value, [tpl.key]: await install.mutateAsync(tpl.key) };
  } catch {
    // shown from install.error
  } finally {
    installing.value = null;
  }
}

const total = (c: Counts) => PARTS.reduce((n, p) => n + c[p], 0);
const buttonLabel = (tpl: Template) =>
  tpl.status === "not_installed"
    ? t("templates.install", { name: tpl.name })
    : tpl.status === "partial"
      ? t("templates.addMissing", { name: tpl.name })
      : t("templates.status.installed");
const addedText = (r: InstallResult) =>
  total(r.existing) > 0
    ? t("templates.result.addedExisting", { n: total(r.created), existing: total(r.existing) })
    : t("templates.result.added", { n: total(r.created) });
</script>

<template>
  <div class="record-head record-head-plain">
    <Breadcrumbs :items="adminCrumbs('templates')" />
    <div class="page-header record-header">
      <div class="record-heading">
        <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="layers" class="class-icon" /></span>
        <div class="record-title">
          <div class="title">
            <h1>{{ t("admin.section.templates") }}</h1>
          </div>
          <p v-if="templates.data.value" class="record-meta" data-testid="record-meta">
            <span class="badge">{{ t("templates.meta.count", { n: templates.data.value.length }) }}</span>
            <span v-if="templates.data.value.length > 0" :class="['badge', installedCount > 0 ? 'ok' : 'off']"
              ><span class="status-dot" aria-hidden="true" />{{ t("templates.meta.installed", { n: installedCount, total: templates.data.value.length }) }}</span
            >
            <span class="record-meta-line">{{ t("templates.meta.idempotent") }}</span>
          </p>
        </div>
      </div>
    </div>
  </div>

  <LoadingState v-if="templates.isLoading.value" :label="t('templates.loading')" />
  <ErrorAlert v-else-if="templates.isError.value" :error="templates.error.value" :on-retry="() => templates.refetch()" />
  <template v-else-if="templates.data.value">
    <section v-if="emptyInstall && templates.data.value.length > 0" class="panel callout">
      <EmptyState :title="t('templates.empty.title')">
        {{ emptyBody[0] }}<RouterLink to="/admin/classes">{{ t("admin.section.classes") }}</RouterLink>{{ emptyBody[1]
        }}<RouterLink to="/admin/dropdowns">{{ t("admin.section.dropdowns") }}</RouterLink>{{ emptyBody[2] }}
      </EmptyState>
    </section>
    <p v-else-if="templates.data.value.length > 0" class="muted">{{ t("templates.intro") }}</p>

    <section v-for="tpl in templates.data.value" :key="tpl.key" class="panel" :aria-labelledby="`tpl-${tpl.key}`">
      <div class="panel-header">
        <div class="template-title">
          <h2 :id="`tpl-${tpl.key}`">{{ tpl.name }}</h2>
          <span :class="['badge', STATUS_TONE[tpl.status]]"><span class="status-dot" aria-hidden="true" />{{ t(STATUS_LABEL[tpl.status]) }}</span>
        </div>
        <button type="button" class="btn btn-primary" :disabled="tpl.status === 'installed' || installing !== null" @click="run(tpl)">
          {{ installing === tpl.key ? t("templates.installing") : buttonLabel(tpl) }}
        </button>
      </div>
      <div class="panel-body stack">
        <p class="no-margin">{{ tpl.description }}</p>
        <ErrorAlert
          v-if="install.isError.value && install.variables.value === tpl.key"
          :error="install.error.value"
          :title="t('templates.installFailed')"
        />
        <div v-if="results[tpl.key]" class="alert template-result" role="status">
          <strong>{{ t("templates.result.title", { name: tpl.name }) }}</strong>
          {{ addedText(results[tpl.key]) }}
          <template v-if="results[tpl.key].skipped.length > 0">
            {{ t("templates.result.skipped") }}
            <ul>
              <li v-for="s in results[tpl.key].skipped" :key="s">{{ s }}</li>
            </ul>
          </template>
          <details v-if="results[tpl.key].schemaChange" class="import-changes">
            <summary>{{ t("templates.result.schemaChange", { summary: results[tpl.key].schemaChange!.summary }) }}</summary>
            <ol class="sc-ddl">
              <li v-for="(sql, i) in results[tpl.key].schemaChange!.statements" :key="i"><pre>{{ sql }}</pre></li>
            </ol>
          </details>
          <div class="actions">
            <RouterLink class="btn btn-sm" to="/admin/classes">{{ t("templates.result.reviewClasses") }}</RouterLink>
            <RouterLink class="btn btn-sm" to="/cis/new">{{ t("templates.result.firstCi") }}</RouterLink>
          </div>
        </div>
        <div class="grid-2">
          <table class="data">
            <caption class="sr-only">{{ t("templates.contents.caption", { name: tpl.name }) }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t("templates.contents.part") }}</th>
                <th scope="col" class="num">{{ t("templates.contents.inTemplate") }}</th>
                <th scope="col" class="num">{{ t("templates.contents.present") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="p in PARTS" :key="p">
                <th scope="row">{{ t(PART_LABEL[p]) }}</th>
                <td class="num mono">{{ tpl.contents[p] }}</td>
                <td class="num">
                  <span :class="tpl.present[p] >= tpl.contents[p] ? 'badge ok' : tpl.present[p] > 0 ? 'badge warn' : 'muted mono'">
                    {{ tpl.present[p] }}
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
          <table class="data">
            <caption class="sr-only">{{ t("templates.classes.caption", { name: tpl.name }) }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t("templates.classes.class") }}</th>
                <th scope="col" class="num">{{ t("templates.classes.own") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="c in tpl.classes" :key="c.key">
                <th scope="row">
                  {{ c.name }}
                  <span v-if="c.isAbstract" class="badge warn spaced" :title="t('templates.classes.abstractHint')">{{ t("templates.classes.abstract") }}</span>
                </th>
                <td class="num mono">{{ c.attributeCount }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </section>
    <EmptyState v-if="templates.data.value.length === 0" :title="t('templates.none')" />
  </template>
</template>
