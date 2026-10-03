<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import CiLink from "../../components/CiLink.vue";
import CiStateBadge from "../../components/CiStateBadge.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import { parentIndex, visibleRows, type TreeRow } from "../../lib/graphTree";
import type { TrailStep } from "../../lib/trail";
import Icon from "../../components/Icon.vue";

/**
 * An indented tree of CIs under a root: the relationship map (lib/graphTree `graphRows`) and the
 * impact analysis's tree view. A WAI-ARIA tree: one stop in the tab order, then the arrow keys move
 * between rows, Right and Left expand and collapse, Home and End go to the first and last row, and
 * Enter opens the row's CI. Tab from a row reaches its links (the CI, and the `actions` slot's).
 */
const props = defineProps<{
  rows: TreeRow[];
  root: { label: string; className: string };
  /** The tree's accessible name. */
  label: string;
  self: TrailStep;
  trail: TrailStep[];
}>();

const collapsed = ref(new Set<string>());
const shown = computed(() => visibleRows(props.rows, collapsed.value));
const activeKey = ref<string | null>(null);
const active = computed(() => (shown.value.some((r) => r.key === activeKey.value) ? activeKey.value : (shown.value[0]?.key ?? null)));
watch(
  () => props.rows,
  () => (collapsed.value = new Set()),
);

const list = ref<HTMLElement | null>(null);
function focusRow(key: string) {
  activeKey.value = key;
  void nextTick(() => list.value?.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`)?.focus());
}
function toggle(r: TreeRow, open?: boolean) {
  const next = new Set(collapsed.value);
  if (open ?? next.has(r.key)) next.delete(r.key);
  else next.add(r.key);
  collapsed.value = next;
}

function onKey(e: KeyboardEvent, r: TreeRow) {
  // Keys typed on a link inside the row are the link's.
  if (e.target !== e.currentTarget) return;
  const rows = shown.value;
  const i = rows.findIndex((x) => x.key === r.key);
  const isOpen = r.hasChildren && !collapsed.value.has(r.key);
  let to: number | null = null;
  switch (e.key) {
    case "ArrowDown":
      to = Math.min(i + 1, rows.length - 1);
      break;
    case "ArrowUp":
      to = Math.max(i - 1, 0);
      break;
    case "Home":
      to = 0;
      break;
    case "End":
      to = rows.length - 1;
      break;
    case "ArrowRight":
      if (r.hasChildren && !isOpen) toggle(r, true);
      else if (isOpen) to = i + 1;
      break;
    case "ArrowLeft":
      if (isOpen) toggle(r, false);
      else to = parentIndex(rows, i) >= 0 ? parentIndex(rows, i) : null;
      break;
    case "Enter":
      (e.currentTarget as HTMLElement).querySelector<HTMLAnchorElement>("a[data-ci-link]")?.click();
      break;
    default:
      return;
  }
  e.preventDefault();
  if (to !== null && rows[to]) focusRow(rows[to].key);
}
</script>

<template>
  <div class="graph-tree">
    <div class="graph-tree-root">
      <bdi>{{ root.label }}</bdi> <span class="muted">(<bdi>{{ root.className }}</bdi>)</span>
    </div>
    <ul ref="list" role="tree" :aria-label="label">
      <li
        v-for="r in shown"
        :key="r.key"
        :data-key="r.key"
        role="treeitem"
        :aria-level="r.level"
        :aria-expanded="r.hasChildren ? !collapsed.has(r.key) : undefined"
        :aria-selected="r.key === active"
        :tabindex="r.key === active ? 0 : -1"
        :style="{ paddingLeft: `${(r.level - 1) * 22}px` }"
        @keydown="onKey($event, r)"
        @focus="activeKey = r.key"
      >
        <button
          v-if="r.hasChildren"
          type="button"
          class="tree-toggle"
          tabindex="-1"
          :aria-label="collapsed.has(r.key) ? `Expand ${r.node.label}` : `Collapse ${r.node.label}`"
          @click="toggle(r)"
        >
          <Icon :name="collapsed.has(r.key) ? 'chevron-right' : 'chevron-down'" :size="14" />
        </button>
        <span v-else class="tree-toggle" aria-hidden="true" />
        <span class="muted"><bdi>{{ r.edgeLabel }}</bdi> → </span>
        <CiLink :id="r.node.id" :from="self" :trail="trail" :tabindex="r.key === active ? 0 : -1" data-ci-link>{{ r.node.label }}</CiLink>
        {{ " " }}<span class="muted" dir="auto">{{ r.node.className }}</span>
        <template v-if="r.criticality !== undefined">{{ " " }}<CriticalityBadge :value="r.criticality" /></template>
        {{ " " }}<CiStateBadge :ci="r.node" />
        <span v-if="r.repeat" class="muted"> (shown above)</span>
        <span v-if="r.note" class="muted"> · {{ r.note }}</span>
        <slot name="actions" :row="r" :tabindex="r.key === active ? 0 : -1" />
      </li>
    </ul>
  </div>
</template>
