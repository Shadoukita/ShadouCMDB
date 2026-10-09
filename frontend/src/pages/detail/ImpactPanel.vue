<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { useRelTypeList } from "../../api/datamodel";
import { downloadImpactCsv, useImpact, useImpactSettings, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t, tAround } from "../../i18n";
import { useDebounced } from "../../lib/composables";
import { impactRows } from "../../lib/graphTree";
import {
  criticalityCounts,
  DEFAULT_STATE,
  DIRECTIONS,
  GROUPS,
  impactParams,
  impactQuery,
  impactTree,
  PARAM_KEYS,
  parseImpactQuery,
  STATE_LABELS,
  summaryPhrase,
  truncationMessage,
  type ImpactState,
} from "../../lib/impact";
import type { TrailStep } from "../../lib/trail";
import { useSessionStore } from "../../stores/session";
import GraphTree from "./GraphTree.vue";
import ImpactList from "./ImpactList.vue";
import ImpactServicesSection from "./ImpactServicesSection.vue";

/**
 * The Impact tab (/cis/:id/impact): which CIs are affected if this one fails (downstream) and
 * which it depends on (upstream), from GET /configuration-items/{id}/impact. Every control lives
 * in the URL (lib/impact), so a view can be bookmarked, shared and walked back with Back; a
 * changed control re-runs the analysis after 300 ms, with no Run button.
 */
const props = defineProps<{ ci: Ci; self: TrailStep; trail: TrailStep[]; defaultDirection?: ImpactState["direction"] }>();
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const settings = useImpactSettings();
const relTypes = useRelTypeList();

const maxDepth = computed(() => settings.data.value?.maxDepth);
/** The defaults a plain link means: Downstream, or Upstream on a business service ("what can take it down"). */
const defaults = computed<ImpactState>(() => ({ ...DEFAULT_STATE, direction: props.defaultDirection ?? DEFAULT_STATE.direction }));
const parsed = computed(() => parseImpactQuery(route.query, maxDepth.value, defaults.value));
const state = computed(() => parsed.value.state);
const notice = ref<string | null>(null);
// "Analyse impact" on another CI reuses this panel: a notice about the previous link does not carry over.
watch(
  () => props.ci.id,
  () => (notice.value = null),
);

function setState(patch: Partial<ImpactState>, replace = false) {
  const next = { ...state.value, ...patch };
  const to = { path: route.path, query: impactQuery(next, defaults.value) };
  return replace ? router.replace(to) : router.push(to);
}
const resetNotice = (keys: (keyof ImpactState)[]) => t("impact.resetNotice", { fields: keys.map((k) => STATE_LABELS[k]).join(", "), n: keys.length });

// A hand-edited or outdated link: the unusable parameters fall back to their defaults, and the tab says so.
watch(
  () => [parsed.value, settings.isFetched.value] as const,
  ([p, settled]) => {
    // Judge the depth against the server's limit, once it is known.
    if (!settled || p.invalid.length === 0) return;
    notice.value = resetNotice(p.invalid as (keyof ImpactState)[]);
    void setState({}, true);
  },
  { immediate: true },
);

/** Relationship types that propagate impact (the choices), plus any the URL names that do not. */
const typeChoices = computed(() =>
  (relTypes.data.value?.data ?? []).filter((rt) => rt.impactDirection !== "none" || !!state.value.types?.includes(rt.id)),
);
const allTypes = computed(() => state.value.types === null);
const typeChecked = (id: string) => allTypes.value || !!state.value.types?.includes(id);
function toggleType(id: string, on: boolean) {
  const current = state.value.types ?? typeChoices.value.map((rt) => rt.id);
  const next = on ? [...new Set([...current, id])] : current.filter((x) => x !== id);
  // Every propagating type chosen is the default: keep the URL short.
  const all = typeChoices.value.every((rt) => next.includes(rt.id)) && next.every((id) => typeChoices.value.some((rt) => rt.id === id));
  // Unchecking the last one leaves none chosen, which is not the same as all.
  void setState({ types: all && next.length > 0 ? null : next });
}
const typesSummary = computed(() => {
  if (allTypes.value) return t("impact.types.all");
  const names = typeChoices.value.filter((rt) => !!state.value.types?.includes(rt.id)).map((rt) => rt.name);
  return names.length === 0 ? t("impact.types.none") : names.length <= 2 ? names.join(", ") : t("impact.types.count", { n: names.length });
});
const noTypeChosen = computed(() => state.value.types?.length === 0);

const depthOptions = computed(() => Array.from({ length: maxDepth.value ?? Math.max(10, state.value.depth) }, (_, i) => i + 1));

// ---------- The analysis ----------
const notConfigured = computed(() => settings.data.value?.anyTypePropagates === false);
const params = computed(() => impactParams(state.value));
const debouncedParams = useDebounced(params, 300);
const impact = useImpact(
  () => props.ci.id,
  debouncedParams,
  () => settings.isFetched.value && !notConfigured.value && !noTypeChosen.value,
);
const data = computed(() => impact.data.value);
/** Whether the result on screen answers the controls as they are now. */
const stale = computed(() => impact.isPlaceholderData.value || JSON.stringify(params.value) !== JSON.stringify(debouncedParams.value));

// "Analysing impact…", and after a second a word that large graphs take longer.
const slow = ref(false);
let slowTimer: ReturnType<typeof setTimeout> | undefined;
watch(
  () => impact.isFetching.value,
  (fetching) => {
    clearTimeout(slowTimer);
    slow.value = false;
    if (fetching) slowTimer = setTimeout(() => (slow.value = true), 1000);
  },
  { immediate: true },
);
onBeforeUnmount(() => clearTimeout(slowTimer));
const analysingText = computed(() => (slow.value ? t("impact.analysingSlow") : t("impact.analysing")));

const apiError = computed(() => (impact.error.value instanceof ApiError ? impact.error.value : null));
const busy = computed(() => !!apiError.value && (apiError.value.code === "RATE_LIMITED" || apiError.value.code === "SERVER_BUSY"));
const missing = computed(() => apiError.value?.code === "NOT_FOUND");
// The API refused a parameter (a hand-edited link): reset that control and say so.
watch(apiError, (e) => {
  if (!e || (e.code !== "VALIDATION_ERROR" && e.code !== "VALIDATION_FAILED")) return;
  const keys = [...new Set(e.details.map((d) => PARAM_KEYS[d.field]).filter((k): k is keyof ImpactState => !!k))];
  if (keys.length === 0) return;
  const patch: Partial<ImpactState> = Object.fromEntries(keys.map((k) => [k, defaults.value[k]]));
  if (JSON.stringify(impactQuery({ ...state.value, ...patch }, defaults.value)) === JSON.stringify(impactQuery(state.value, defaults.value))) return;
  notice.value = resetNotice(keys);
  void setState(patch, true);
});
const validationHandled = computed(
  () => !!apiError.value && apiError.value.details.some((d) => PARAM_KEYS[d.field]) && notice.value !== null,
);

// ---------- Header ----------
/** The summary after its count: "of <root> within 2 hops", with the root's name between the two parts. */
const summaryParts = computed(() =>
  data.value
    ? tAround(data.value.parameters.direction === "both" ? "impact.summary.around" : "impact.summary.of", "name", { n: data.value.parameters.depth })
    : ["", ""],
);
const EMPTY_TITLES = { downstream: "impact.empty.downstream", upstream: "impact.empty.upstream", both: "impact.empty.around" } as const;
const emptyTitle = computed(() =>
  data.value ? t(EMPTY_TITLES[data.value.parameters.direction], { name: data.value.root.name, n: data.value.parameters.depth }) : "",
);
const notConfiguredParts = computed(() => tAround("impact.notConfigured.admin", "link"));
const critCounts = computed(() => (data.value ? criticalityCounts(data.value) : ""));
const beyondDepth = computed(() => !!data.value?.hasMoreBeyondDepth && !data.value.truncated);
const canDeepen = computed(() => !!data.value && data.value.parameters.depth < (maxDepth.value ?? data.value.limits.maxDepth));
const canManage = computed(() => session.can("datamodel.manage"));

// ---------- Tree ----------
const subtrees = computed(() => (data.value ? impactTree(data.value).map((st) => ({ ...st, rows: impactRows(st.rows) })) : []));

// ---------- Export ----------
const exporting = ref(false);
const exportError = ref<unknown>(null);
async function exportCsv() {
  exporting.value = true;
  exportError.value = null;
  try {
    const now = new Date();
    const pad = (n: number) => String(n).padStart(2, "0");
    const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}`;
    await downloadImpactCsv(props.ci.id, params.value, `impact-${props.ci.ident}-${state.value.direction}-${stamp}.csv`);
  } catch (e) {
    exportError.value = e;
  } finally {
    exporting.value = false;
  }
}

const VIEWS = [
  { value: "list", label: "impact.view.list" },
  { value: "tree", label: "impact.view.tree" },
] as const;
</script>

<template>
  <section class="panel impact" aria-labelledby="impact-title">
    <h2 id="impact-title" class="sr-only">{{ t("record.actions.impact") }}</h2>
    <form class="toolbar impact-controls" :aria-label="t('impact.options')" @submit.prevent>
      <fieldset class="field impact-direction" role="radiogroup" aria-labelledby="impact-direction-label">
        <legend id="impact-direction-label" class="label">{{ t("impact.direction") }}</legend>
        <div class="segmented">
          <label v-for="d in DIRECTIONS" :key="d.value" :title="d.hint">
            <input
              type="radio"
              name="impact-direction"
              :value="d.value"
              :checked="state.direction === d.value"
              @change="setState({ direction: d.value })"
            />
            {{ d.label }} <span class="segment-hint">{{ d.hint }}</span>
          </label>
        </div>
      </fieldset>
      <div class="field">
        <label for="impact-depth">{{ t("impact.depth") }}</label>
        <select id="impact-depth" :value="state.depth" @change="setState({ depth: Number(($event.target as HTMLSelectElement).value) })">
          <option v-for="d in depthOptions" :key="d" :value="d">
            {{ d === maxDepth && d > 1 ? t("impact.depthOptionMax", { n: d }) : t("impact.depthOption", { n: d }) }}
          </option>
        </select>
      </div>
      <details class="field impact-types">
        <summary>
          <span class="label">{{ t("impact.types") }}</span>
          <span class="select-like">{{ typesSummary }}</span>
        </summary>
        <fieldset class="popover">
          <legend class="sr-only">{{ t("impact.types.legend") }}</legend>
          <LoadingState v-if="relTypes.isLoading.value" :label="t('common.loading')" />
          <p v-else-if="relTypes.isError.value" class="muted">{{ t("impact.types.failed") }}</p>
          <p v-else-if="typeChoices.length === 0" class="muted">{{ t("impact.types.empty") }}</p>
          <label v-for="rt in typeChoices" :key="rt.id" class="checkbox-row">
            <input type="checkbox" :checked="typeChecked(rt.id)" @change="toggleType(rt.id, ($event.target as HTMLInputElement).checked)" />
            <bdi>{{ rt.name }}</bdi>
            <span v-if="!rt.isActive" class="muted">{{ t("filters.retired") }}</span>
            <span v-if="rt.impactDirection === 'none'" class="muted">{{ t("impact.types.noPropagation") }}</span>
          </label>
          <button v-if="!allTypes" type="button" class="btn btn-sm" @click="setState({ types: null })">{{ t("impact.types.all") }}</button>
        </fieldset>
      </details>
      <div class="field">
        <span class="label">{{ t("impact.inactive") }}</span>
        <label class="checkbox-row">
          <input id="impact-inactive" type="checkbox" :checked="state.includeInactive" @change="setState({ includeInactive: ($event.target as HTMLInputElement).checked })" />
          {{ t("impact.includeInactive") }}
        </label>
      </div>
      <div v-if="state.view === 'list'" class="field">
        <label for="impact-group">{{ t("impact.groupBy") }}</label>
        <select id="impact-group" :value="state.group" @change="setState({ group: ($event.target as HTMLSelectElement).value as ImpactState['group'] })">
          <option v-for="g in GROUPS" :key="g.value" :value="g.value">{{ g.label }}</option>
        </select>
      </div>
      <div class="toolbar-end impact-actions">
        <span v-if="impact.isFetching.value && data" class="spinner" aria-hidden="true" />
        <button
          type="button"
          class="btn"
          :disabled="exporting || notConfigured || noTypeChosen"
          :title="t('impact.exportTitle')"
          @click="exportCsv"
        >
          {{ exporting ? t("impact.exporting") : t("impact.export") }}
        </button>
      </div>
    </form>

    <div v-if="notice" class="panel-body">
      <div class="alert alert-warn" role="status">
        {{ notice }} <button type="button" class="btn btn-sm" @click="notice = null">{{ t("impact.dismiss") }}</button>
      </div>
    </div>
    <div v-if="exportError" class="panel-body">
      <ErrorAlert :error="exportError" :title="t('impact.exportFailed')" />
    </div>

    <!-- Nothing propagates impact yet: every analysis would be empty. -->
    <EmptyState v-if="notConfigured" :title="t('topology.impact.notConfigured')">
      <template v-if="canManage">
        {{ notConfiguredParts[0] }}<RouterLink to="/admin/relationships">{{ t("impact.notConfigured.link") }}</RouterLink>{{ notConfiguredParts[1] }}
      </template>
      <template v-else>
        {{ t("impact.notConfigured.user") }}
      </template>
    </EmptyState>
    <div v-else-if="settings.isError.value" class="panel-body">
      <ErrorAlert :error="settings.error.value" :title="t('impact.settingsFailed')" :on-retry="() => settings.refetch()" />
    </div>
    <EmptyState v-else-if="noTypeChosen" :title="t('impact.noTypeChosen.title')">
      {{ t("impact.noTypeChosen.body") }}
      <template #actions><button type="button" class="btn" @click="setState({ types: null })">{{ t("impact.followAll") }}</button></template>
    </EmptyState>

    <template v-else>
      <!-- Errors: missing CI, busy server, anything else with the API's message and request id. -->
      <EmptyState v-if="missing" :title="t('record.notFound.title')">
        {{ t("impact.notFound.body") }}
        <template #actions><RouterLink class="btn" to="/cis">{{ t("inventory.denied.back") }}</RouterLink></template>
      </EmptyState>
      <div v-else-if="busy" class="panel-body">
        <div class="alert alert-warn" role="alert">
          <strong>{{ t("impact.busy.title") }}</strong>
          <div>{{ t("impact.busy.body") }}</div>
          <div class="meta">
            <template v-if="apiError?.requestId">{{ t("error.requestId") }} <code>{{ apiError.requestId }}</code>&#32;</template>
            <button type="button" class="btn btn-sm" @click="impact.refetch()">{{ t("common.retry") }}</button>
          </div>
        </div>
      </div>
      <div v-else-if="impact.isError.value && !validationHandled" class="panel-body">
        <ErrorAlert :error="impact.error.value" :title="t('impact.failed')" :on-retry="() => impact.refetch()" />
      </div>

      <!-- Loading: the header and a skeleton of the table. -->
      <div v-if="!data && (impact.isLoading.value || !settings.isFetched.value)" class="panel-body">
        <p class="impact-summary" role="status" aria-live="polite"><span class="spinner" aria-hidden="true" /> {{ analysingText }}</p>
        <div class="skeleton-table" aria-hidden="true">
          <div v-for="n in 6" :key="n" class="skeleton-row" />
        </div>
      </div>

      <template v-if="data && !missing && !busy">
        <div class="panel-body impact-header">
          <p class="impact-summary" role="status" aria-live="polite">
            <template v-if="impact.isFetching.value && stale">{{ analysingText }}</template>
            <template v-else>
              <strong>{{ summaryPhrase(data.summary.total, data.parameters.direction) }}</strong>
              {{ summaryParts[0] }}<em dir="auto">{{ data.root.name }}</em>{{ summaryParts[1] }}<template v-if="critCounts"> · {{ critCounts }}</template>
            </template>
          </p>
          <p v-if="data.visibility === 'restricted'" class="muted impact-visibility">{{ t("impact.visibilityNote") }}</p>
          <div v-if="data.truncated" class="alert alert-warn" role="status">
            <strong>{{ t("impact.incomplete") }}</strong> {{ truncationMessage(data, settings.data.value?.timeoutMs) }}
          </div>
          <p v-if="beyondDepth" class="impact-beyond">
            {{ t("impact.beyond", { n: data.parameters.depth }) }}
            <button v-if="canDeepen" type="button" class="btn btn-sm" @click="setState({ depth: data.parameters.depth + 1 })">{{ t("impact.increaseDepth") }}</button>
            <span v-else class="muted">{{ t("impact.maxDepthReached") }}</span>
          </p>
        </div>

        <EmptyState v-if="data.items.length === 0" :title="emptyTitle">
          <template v-if="data.parameters.direction !== 'both'">{{ data.parameters.direction === "downstream" ? t("impact.empty.tryUpstream") : t("impact.empty.tryDownstream") }}</template>
          <template v-else>{{ t("impact.empty.tryMore") }}</template>
        </EmptyState>

        <template v-else>
          <!-- List or tree: a segmented control, not a second tab bar under the page's tabs (audit R7). -->
          <div class="impact-views">
            <div class="segmented" role="radiogroup" :aria-label="t('impact.view')">
              <label v-for="v in VIEWS" :key="v.value">
                <input type="radio" name="impact-view" :value="v.value" :checked="state.view === v.value" @change="setState({ view: v.value })" />{{ t(v.label) }}
              </label>
            </div>
          </div>
          <div id="impact-view-panel" :class="{ loading: stale }" :aria-busy="stale">
            <template v-if="state.view === 'list'">
              <ImpactServicesSection :analysis="data" :self="self" :trail="trail" />
              <ImpactList :analysis="data" :group="state.group" :sort="state.sort" :self="self" :trail="trail" @sort="(s) => setState({ sort: s })" />
            </template>
            <div v-else class="panel-body impact-trees">
              <div v-for="st in subtrees" :key="st.way">
                <h3 v-if="subtrees.length > 1">{{ st.title }}</h3>
                <p v-if="st.rows.length === 0" class="muted">{{ t("impact.tree.none", { n: data.parameters.depth }) }}</p>
                <GraphTree
                  v-else
                  :rows="st.rows"
                  :root="{ label: data.root.name, className: data.root.className, classId: ci.classId }"
                  :label="t('impact.tree.label', { title: st.title, name: data.root.name })"
                  :self="self"
                  :trail="trail"
                />
              </div>
            </div>
          </div>
        </template>
      </template>
    </template>
  </section>
</template>
