<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { useRelTypeList } from "../../api/datamodel";
import { downloadImpactCsv, useImpact, useImpactSettings, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { useDebounced } from "../../lib/composables";
import { plural } from "../../lib/format";
import type { TreeRow } from "../../lib/graphTree";
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
  VISIBILITY_NOTE,
  type ImpactState,
} from "../../lib/impact";
import type { TrailStep } from "../../lib/trail";
import { useSessionStore } from "../../stores/session";
import GraphTree from "./GraphTree.vue";
import ImpactList from "./ImpactList.vue";

/**
 * The Impact tab (/cis/:id/impact): which CIs are affected if this one fails (downstream) and
 * which it depends on (upstream), from GET /configuration-items/{id}/impact. Every control lives
 * in the URL (lib/impact), so a view can be bookmarked, shared and walked back with Back; a
 * changed control re-runs the analysis after 300 ms, with no Run button.
 */
const props = defineProps<{ ci: Ci; self: TrailStep; trail: TrailStep[] }>();
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const settings = useImpactSettings();
const relTypes = useRelTypeList();

const maxDepth = computed(() => settings.data.value?.maxDepth);
const parsed = computed(() => parseImpactQuery(route.query, maxDepth.value));
const state = computed(() => parsed.value.state);
const notice = ref<string | null>(null);
// "Analyse impact" on another CI reuses this panel: a notice about the previous link does not carry over.
watch(
  () => props.ci.id,
  () => (notice.value = null),
);

function setState(patch: Partial<ImpactState>, replace = false) {
  const next = { ...state.value, ...patch };
  const to = { path: route.path, query: impactQuery(next) };
  return replace ? router.replace(to) : router.push(to);
}
const resetNotice = (keys: (keyof ImpactState)[]) =>
  `The link's ${keys.map((k) => STATE_LABELS[k]).join(", ")} could not be used and ${keys.length === 1 ? "was" : "were"} reset to the default.`;

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
  (relTypes.data.value?.data ?? []).filter((t) => t.impactDirection !== "none" || state.value.types.includes(t.id)),
);
const allTypes = computed(() => state.value.types.length === 0);
const typeChecked = (id: string) => allTypes.value || state.value.types.includes(id);
function toggleType(id: string, on: boolean) {
  const current = allTypes.value ? typeChoices.value.map((t) => t.id) : state.value.types;
  const next = on ? [...new Set([...current, id])] : current.filter((t) => t !== id);
  // Every propagating type chosen is the default: keep the URL short.
  const all = typeChoices.value.every((t) => next.includes(t.id)) && next.every((id) => typeChoices.value.some((t) => t.id === id));
  void setState({ types: all ? [] : next });
}
const typesSummary = computed(() => {
  if (allTypes.value) return "All propagating types";
  const names = typeChoices.value.filter((t) => state.value.types.includes(t.id)).map((t) => t.name);
  return names.length === 0 ? "None" : names.length <= 2 ? names.join(", ") : `${names.length} types`;
});
const noTypeChosen = computed(() => !allTypes.value && state.value.types.length === 0);

const depthOptions = computed(() => Array.from({ length: maxDepth.value ?? Math.max(10, state.value.depth) }, (_, i) => i + 1));

// ---------- The analysis ----------
const notConfigured = computed(() => settings.data.value?.anyTypePropagates === false);
const params = computed(() => impactParams(state.value));
const debouncedParams = useDebounced(params, 300);
const impact = useImpact(
  () => props.ci.id,
  debouncedParams,
  () => settings.isFetched.value && !notConfigured.value,
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
const analysingText = computed(() => (slow.value ? "Analysing impact… this can take a few seconds on large graphs" : "Analysing impact…"));

const apiError = computed(() => (impact.error.value instanceof ApiError ? impact.error.value : null));
const busy = computed(() => !!apiError.value && (apiError.value.code === "RATE_LIMITED" || apiError.value.code === "SERVER_BUSY"));
const missing = computed(() => apiError.value?.code === "NOT_FOUND");
// The API refused a parameter (a hand-edited link): reset that control and say so.
watch(apiError, (e) => {
  if (!e || (e.code !== "VALIDATION_ERROR" && e.code !== "VALIDATION_FAILED")) return;
  const keys = [...new Set(e.details.map((d) => PARAM_KEYS[d.field]).filter((k): k is keyof ImpactState => !!k))];
  if (keys.length === 0) return;
  const patch: Partial<ImpactState> = Object.fromEntries(keys.map((k) => [k, DEFAULT_STATE[k]]));
  if (JSON.stringify(impactQuery({ ...state.value, ...patch })) === JSON.stringify(impactQuery(state.value))) return;
  notice.value = resetNotice(keys);
  void setState(patch, true);
});
const validationHandled = computed(
  () => !!apiError.value && apiError.value.details.some((d) => PARAM_KEYS[d.field]) && notice.value !== null,
);

// ---------- Header ----------
const directionWord = computed(() => (data.value ? { downstream: "downstream of", upstream: "upstream of", both: "around" }[data.value.parameters.direction] : ""));
const critCounts = computed(() => (data.value ? criticalityCounts(data.value) : ""));
const beyondDepth = computed(() => !!data.value?.hasMoreBeyondDepth && !data.value.truncated);
const canDeepen = computed(() => !!data.value && data.value.parameters.depth < (maxDepth.value ?? data.value.limits.maxDepth));
const canManage = computed(() => session.can("datamodel.manage"));

// ---------- Tree ----------
const subtrees = computed(() =>
  data.value
    ? impactTree(data.value).map((t) => ({
        ...t,
        rows: t.rows.map(
          (r): TreeRow => ({
            key: r.key,
            level: r.level,
            edgeLabel: r.label,
            node: { id: r.item.id, label: r.item.name, className: r.item.className, active: r.item.active },
            hasChildren: r.hasChildren,
            criticality: r.item.criticality,
            note: r.item.reachedByCount > 1 ? `also reached via ${plural(r.item.reachedByCount - 1, "other relationship")}` : undefined,
          }),
        ),
      }))
    : [],
);

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
  { value: "list", label: "List" },
  { value: "tree", label: "Tree" },
] as const;
/** Arrow keys, Home and End move between the two views. */
async function onViewKey(e: KeyboardEvent) {
  if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
  e.preventDefault();
  const view = e.key === "Home" ? "list" : e.key === "End" ? "tree" : state.value.view === "list" ? "tree" : "list";
  await setState({ view });
  await nextTick();
  document.getElementById(`impact-view-${view}`)?.focus();
}
</script>

<template>
  <section class="panel impact" aria-labelledby="impact-title">
    <h2 id="impact-title" class="sr-only">Impact analysis</h2>
    <form class="toolbar impact-controls" aria-label="Impact analysis options" @submit.prevent>
      <fieldset class="field impact-direction" role="radiogroup" aria-labelledby="impact-direction-label">
        <legend id="impact-direction-label" class="label">Direction</legend>
        <div class="segmented">
          <label v-for="d in DIRECTIONS" :key="d.value" :title="d.hint">
            <input
              type="radio"
              name="impact-direction"
              :value="d.value"
              :checked="state.direction === d.value"
              @change="setState({ direction: d.value })"
            />
            <span>{{ d.label }} <span class="muted">· {{ d.hint }}</span></span>
          </label>
        </div>
      </fieldset>
      <div class="field">
        <label for="impact-depth">Depth</label>
        <select id="impact-depth" :value="state.depth" @change="setState({ depth: Number(($event.target as HTMLSelectElement).value) })">
          <option v-for="d in depthOptions" :key="d" :value="d">
            {{ d === maxDepth && d > 1 ? `Maximum (${plural(d, "hop")})` : plural(d, "hop") }}
          </option>
        </select>
      </div>
      <details class="field impact-types">
        <summary>
          <span class="label">Relationship types</span>
          <span class="select-like">{{ typesSummary }}</span>
        </summary>
        <fieldset class="popover">
          <legend class="sr-only">Relationship types to follow</legend>
          <p v-if="relTypes.isLoading.value" class="muted">Loading…</p>
          <p v-else-if="relTypes.isError.value" class="muted">Could not load the relationship types; every propagating type is followed.</p>
          <p v-else-if="typeChoices.length === 0" class="muted">No relationship type propagates impact.</p>
          <label v-for="t in typeChoices" :key="t.id" class="checkbox-row">
            <input type="checkbox" :checked="typeChecked(t.id)" @change="toggleType(t.id, ($event.target as HTMLInputElement).checked)" />
            <bdi>{{ t.name }}</bdi>
            <span v-if="!t.isActive" class="muted">(retired)</span>
            <span v-if="t.impactDirection === 'none'" class="muted">(does not propagate impact)</span>
          </label>
          <button v-if="!allTypes" type="button" class="btn btn-sm" @click="setState({ types: [] })">All propagating types</button>
        </fieldset>
      </details>
      <div class="field">
        <span class="label">Inactive CIs</span>
        <label class="checkbox-row">
          <input id="impact-inactive" type="checkbox" :checked="state.includeInactive" @change="setState({ includeInactive: ($event.target as HTMLInputElement).checked })" />
          Include inactive CIs
        </label>
      </div>
      <div v-if="state.view === 'list'" class="field">
        <label for="impact-group">Group by</label>
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
          title="Download the current result as CSV (recorded in the audit log)"
          @click="exportCsv"
        >
          {{ exporting ? "Preparing…" : "Export CSV" }}
        </button>
      </div>
    </form>

    <div v-if="notice" class="panel-body">
      <div class="alert alert-warn" role="status">
        {{ notice }} <button type="button" class="btn btn-sm" @click="notice = null">Dismiss</button>
      </div>
    </div>
    <div v-if="exportError" class="panel-body">
      <ErrorAlert :error="exportError" title="The CSV export failed" />
    </div>

    <!-- Nothing propagates impact yet: every analysis would be empty. -->
    <EmptyState v-if="notConfigured" title="Impact analysis is not configured">
      <template v-if="canManage">
        No relationship type is set to propagate impact yet. Configure it in
        <RouterLink to="/admin/relationships">Data model › Relationship types</RouterLink>: edit a type and choose its
        impact propagation.
      </template>
      <template v-else>
        Impact analysis is not configured. Ask an administrator to configure which relationship types propagate impact.
      </template>
    </EmptyState>
    <div v-else-if="settings.isError.value" class="panel-body">
      <ErrorAlert :error="settings.error.value" title="Could not load the impact analysis settings" :on-retry="() => settings.refetch()" />
    </div>
    <EmptyState v-else-if="noTypeChosen" title="No relationship type chosen">
      Choose at least one relationship type to follow.
      <template #actions><button type="button" class="btn" @click="setState({ types: [] })">Follow all propagating types</button></template>
    </EmptyState>

    <template v-else>
      <!-- Errors: missing CI, busy server, anything else with the API's message and request id. -->
      <EmptyState v-if="missing" title="Configuration item not found">
        This CI no longer exists, or your profile does not allow viewing it.
        <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
      </EmptyState>
      <div v-else-if="busy" class="panel-body">
        <div class="alert alert-warn" role="alert">
          <strong>The server is busy</strong>
          <div>Too many impact analyses are running. Try again in a moment.</div>
          <div class="meta">
            <template v-if="apiError?.requestId">Request id <code>{{ apiError.requestId }}</code>&#32;</template>
            <button type="button" class="btn btn-sm" @click="impact.refetch()">Retry</button>
          </div>
        </div>
      </div>
      <div v-else-if="impact.isError.value && !validationHandled" class="panel-body">
        <ErrorAlert :error="impact.error.value" title="The impact analysis failed" :on-retry="() => impact.refetch()" />
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
              {{ data.parameters.direction === "both" ? "around" : "of" }}
              <em dir="auto">{{ data.root.name }}</em> within {{ plural(data.parameters.depth, "hop") }}<template v-if="critCounts"> · {{ critCounts }}</template>
            </template>
          </p>
          <p v-if="data.visibility === 'restricted'" class="muted impact-visibility">{{ VISIBILITY_NOTE }}</p>
          <div v-if="data.truncated" class="alert alert-warn" role="status">
            <strong>Incomplete result.</strong> {{ truncationMessage(data, settings.data.value?.timeoutMs) }}
          </div>
          <p v-if="beyondDepth" class="impact-beyond">
            More CIs may be affected beyond {{ plural(data.parameters.depth, "hop") }}.
            <button v-if="canDeepen" type="button" class="btn btn-sm" @click="setState({ depth: data.parameters.depth + 1 })">Increase depth</button>
            <span v-else class="muted">This is the server's largest depth.</span>
          </p>
        </div>

        <EmptyState v-if="data.items.length === 0" :title="`No CIs are affected ${directionWord} ${data.root.name} within ${plural(data.parameters.depth, 'hop')}.`">
          <template v-if="data.parameters.direction !== 'both'">Try {{ data.parameters.direction === "downstream" ? "Upstream" : "Downstream" }} or increase the depth.</template>
          <template v-else>Try a larger depth, or more relationship types.</template>
        </EmptyState>

        <template v-else>
          <div class="tabs impact-views" role="tablist" aria-label="Result view">
            <button
              v-for="v in VIEWS"
              :id="`impact-view-${v.value}`"
              :key="v.value"
              type="button"
              role="tab"
              :aria-selected="state.view === v.value"
              aria-controls="impact-view-panel"
              :tabindex="state.view === v.value ? 0 : -1"
              @click="setState({ view: v.value })"
              @keydown="onViewKey"
            >
              {{ v.label }}
            </button>
          </div>
          <div id="impact-view-panel" role="tabpanel" :aria-labelledby="`impact-view-${state.view}`" :class="{ loading: stale }">
            <ImpactList v-if="state.view === 'list'" :analysis="data" :group="state.group" :sort="state.sort" :self="self" :trail="trail" @sort="(s) => setState({ sort: s })" />
            <div v-else class="panel-body impact-trees">
              <div v-for="t in subtrees" :key="t.way">
                <h3 v-if="subtrees.length > 1">{{ t.title }}</h3>
                <p v-if="t.rows.length === 0" class="muted">None within {{ plural(data.parameters.depth, "hop") }}.</p>
                <GraphTree
                  v-else
                  :rows="t.rows"
                  :root="{ label: data.root.name, className: data.root.className }"
                  :label="`${t.title}: impact tree of ${data.root.name}`"
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
