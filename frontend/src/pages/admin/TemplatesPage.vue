<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useInstallTemplate, useTemplates } from "../../api/datamodel";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import type { SchemaChange } from "../../api/schemaChanges";
import LoadingState from "../../components/LoadingState.vue";
import { useDocumentTitle } from "../../lib/composables";

/**
 * Administration › Data model › Templates. A fresh install has no classes or
 * lookups; a starter template adds a ready-made model in one click. Installing is
 * idempotent: rows that already exist (matched by key) are left alone.
 */
useDocumentTitle("Templates");
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
const PARTS: { key: Part; label: string }[] = [
  { key: "areas", label: "Areas" },
  { key: "classes", label: "CI classes" },
  { key: "attributeDefinitions", label: "Attributes" },
  { key: "relationshipTypes", label: "Relationship types" },
  { key: "relationshipRules", label: "Relationship rules" },
  { key: "lookupLists", label: "Lookup lists" },
  { key: "lookupListValues", label: "Lookup values" },
];
const STATUS: Record<Status, { label: string; tone: string }> = {
  not_installed: { label: "Not installed", tone: "" },
  partial: { label: "Partly installed", tone: "warn" },
  installed: { label: "Installed", tone: "ok" },
};

/** Nothing of any template is present yet: the empty-install state. */
const emptyInstall = computed(() => (templates.data.value ?? []).every((t) => PARTS.every((p) => t.present[p.key] === 0)));

async function run(t: Template) {
  installing.value = t.key;
  install.reset();
  try {
    results.value = { ...results.value, [t.key]: await install.mutateAsync(t.key) };
  } catch {
    // shown from install.error
  } finally {
    installing.value = null;
  }
}

const total = (c: Counts) => PARTS.reduce((n, p) => n + c[p.key], 0);
const buttonLabel = (t: Template) =>
  t.status === "not_installed" ? `Install ${t.name} starter` : t.status === "partial" ? `Add the missing parts of ${t.name}` : "Installed";
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Data model' }, { label: 'Templates' }]" />
  <div class="page-header">
    <div class="title"><h1>Starter templates</h1></div>
  </div>

  <LoadingState v-if="templates.isLoading.value" label="Loading templates…" />
  <ErrorAlert v-else-if="templates.isError.value" :error="templates.error.value" :on-retry="() => templates.refetch()" />
  <template v-else-if="templates.data.value">
    <section v-if="emptyInstall" class="panel callout">
      <EmptyState title="Your CMDB is empty">
        There are no CI classes, relationship types or lookups yet, so nobody can record a configuration item. Install a
        starter template to begin with a ready-made model you can change afterwards, or build your own under
        <RouterLink to="/admin/classes">CI classes</RouterLink> and <RouterLink to="/admin/lookups">Lookups</RouterLink>.
      </EmptyState>
    </section>
    <p v-else class="muted">
      Templates add a ready-made data model. Installing again only adds what is missing: existing classes, attributes and
      lookups (matched by key) are left as they are, including your changes.
    </p>

    <section v-for="t in templates.data.value" :key="t.key" class="panel" :aria-labelledby="`tpl-${t.key}`">
      <div class="panel-header">
        <h2 :id="`tpl-${t.key}`">{{ t.name }}</h2>
        <span :class="['badge', STATUS[t.status].tone]">{{ STATUS[t.status].label }}</span>
        <button
          type="button"
          class="btn btn-primary"
          style="margin-left: auto"
          :disabled="t.status === 'installed' || installing !== null"
          @click="run(t)"
        >
          {{ installing === t.key ? "Installing…" : buttonLabel(t) }}
        </button>
      </div>
      <div class="panel-body">
        <p style="margin-top: 0">{{ t.description }}</p>
        <ErrorAlert v-if="install.isError.value && install.variables.value === t.key" :error="install.error.value" title="Installation failed; nothing was changed" />
        <div v-if="results[t.key]" class="alert" role="status">
          <strong>Installed {{ t.name }}.</strong>
          Added {{ total(results[t.key].created) }} rows<template v-if="total(results[t.key].existing) > 0">; {{ total(results[t.key].existing) }} already existed and were left unchanged</template>.
          <template v-if="results[t.key].skipped.length > 0">
            Skipped because they clash with your data model:
            <ul>
              <li v-for="s in results[t.key].skipped" :key="s">{{ s }}</li>
            </ul>
          </template>
          <details v-if="results[t.key].schemaChange" class="import-changes">
            <summary>Database changes: {{ results[t.key].schemaChange!.summary }}</summary>
            <ol class="sc-ddl">
              <li v-for="(sql, i) in results[t.key].schemaChange!.statements" :key="i"><pre>{{ sql }}</pre></li>
            </ol>
          </details>
          <div class="actions" style="margin-top: var(--sp-3)">
            <RouterLink class="btn btn-sm" to="/admin/classes">Review the CI classes</RouterLink>
            <RouterLink class="btn btn-sm" to="/cis/new">Create the first CI</RouterLink>
          </div>
        </div>
        <div class="grid-2">
          <table class="data">
            <caption class="sr-only">What {{ t.name }} contains</caption>
            <thead>
              <tr>
                <th scope="col">Contains</th>
                <th scope="col" class="num">In template</th>
                <th scope="col" class="num">Already present</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="p in PARTS" :key="p.key">
                <th scope="row">{{ p.label }}</th>
                <td class="num">{{ t.contents[p.key] }}</td>
                <td class="num">
                  <span :class="t.present[p.key] >= t.contents[p.key] ? 'badge ok' : t.present[p.key] > 0 ? 'badge warn' : 'muted'">
                    {{ t.present[p.key] }}
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
          <table class="data">
            <caption class="sr-only">Classes in {{ t.name }}</caption>
            <thead>
              <tr>
                <th scope="col">Class</th>
                <th scope="col" class="num">Own attributes</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="c in t.classes" :key="c.key">
                <th scope="row">
                  {{ c.name }} <span v-if="c.isAbstract" class="badge warn" title="Groups other classes; holds no CIs itself">abstract</span>
                </th>
                <td class="num">{{ c.attributeCount }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </section>
    <EmptyState v-if="templates.data.value.length === 0" title="This server ships no starter templates" />
  </template>
</template>
