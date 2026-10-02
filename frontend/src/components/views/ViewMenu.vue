<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, useId } from "vue";
import type { ApiError } from "../../api/client";
import type { SavedView } from "../../api/savedViews";
import { filterViews, groupViews, MENU_FILTER_THRESHOLD, typeaheadIndex, type ViewActions } from "../../lib/savedViews";

export type ViewAction = keyof ViewActions | "copyLink" | "manage";

/**
 * The View menu (§1.2, §1.6): the user's views and the shared views, then the
 * actions on the current one. A WAI-ARIA menu button: Enter, Space or ↓ opens it
 * on the first item; ↑/↓, Home/End and typing a letter move; Esc closes it and
 * returns focus to the button. With more than 10 views a filter sits on top and
 * the popup is a combobox with a listbox (the actions follow as buttons).
 */
const props = defineProps<{
  views: readonly SavedView[] | undefined;
  loading: boolean;
  error: ApiError | null;
  currentId: string | undefined;
  /** The button's text: the current view's name, "Unsaved view", or "Views…" while loading. */
  label: string;
  actions: ViewActions;
  /** "Server" or "Inventory": the list a default is set for (inventory only). */
  homeName: string | null;
}>();
const emit = defineEmits<{ select: [view: SavedView]; action: [action: ViewAction]; retry: [] }>();

const uid = useId();
const ids = { menu: `view-menu-${uid}`, mine: `view-mine-${uid}`, shared: `view-shared-${uid}`, filter: `view-filter-${uid}`, list: `view-list-${uid}` };
const open = ref(false);
const root = ref<HTMLElement>();
const button = ref<HTMLButtonElement>();
const popup = ref<HTMLElement>();
const filterText = ref("");
const active = ref(-1);

const filterMode = computed(() => (props.views?.length ?? 0) > MENU_FILTER_THRESHOLD);
const shown = computed(() => filterViews(props.views ?? [], filterMode.value ? filterText.value : ""));
const groups = computed(() => groupViews(shown.value));
/** The options of the listbox, in order (filter mode). */
const options = computed(() => [...groups.value.mine, ...groups.value.shared]);
const optionId = (v: SavedView) => `${ids.list}-${v.id}`;

const unavailableHint = "This view refers to a filter that no longer exists.";

interface ActionItem {
  key: ViewAction;
  label: string;
  danger?: boolean;
}
const actionItems = computed<ActionItem[]>(() => {
  const a = props.actions;
  const out: ActionItem[] = [];
  if (a.save) out.push({ key: "save", label: "Save view" });
  if (a.saveAs) out.push({ key: "saveAs", label: "Save as new view…" });
  if (a.rename) out.push({ key: "rename", label: "Rename…" });
  if (a.setDefault) out.push({ key: "setDefault", label: `Set as my default for ${props.homeName ?? "Inventory"}` });
  if (a.clearDefault) out.push({ key: "clearDefault", label: "Clear my default" });
  if (a.copyToMine) out.push({ key: "copyToMine", label: "Copy to my views…" });
  if (a.shareCopy) out.push({ key: "shareCopy", label: "Share a copy…" });
  out.push({ key: "copyLink", label: "Copy link" });
  if (a.delete) out.push({ key: "delete", label: "Delete…", danger: true });
  out.push({ key: "manage", label: "Manage views…" });
  return out;
});

const items = () => [...(popup.value?.querySelectorAll<HTMLElement>("[data-menu-item]") ?? [])];

async function show(at: "first" | "last" = "first") {
  open.value = true;
  filterText.value = "";
  active.value = -1;
  document.addEventListener("pointerdown", onOutside, true);
  await nextTick();
  if (filterMode.value) {
    popup.value?.querySelector<HTMLInputElement>(`#${ids.filter}`)?.focus();
    return;
  }
  const all = items();
  (at === "first" ? all[0] : all[all.length - 1])?.focus();
}
function hide(refocus: boolean) {
  open.value = false;
  document.removeEventListener("pointerdown", onOutside, true);
  if (refocus) button.value?.focus();
}
function onOutside(e: Event) {
  if (!root.value?.contains(e.target as Node)) hide(false);
}
onBeforeUnmount(() => document.removeEventListener("pointerdown", onOutside, true));

function onButtonKey(e: KeyboardEvent) {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    void show(e.key === "ArrowDown" ? "first" : "last");
  }
}

/** Menu keys (§1.6): arrows, Home/End, a letter jumps, Esc closes, Tab leaves. */
function onMenuKey(e: KeyboardEvent) {
  const all = items();
  const at = all.indexOf(document.activeElement as HTMLElement);
  const to = { ArrowDown: at + 1, ArrowUp: at - 1 + all.length, Home: 0, End: all.length - 1 }[e.key];
  if (to !== undefined) {
    e.preventDefault();
    all[to % all.length]?.focus();
  } else if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    hide(true);
  } else if (e.key === "Tab") {
    hide(false);
  } else if (e.key.length === 1 && /\S/.test(e.key) && !e.ctrlKey && !e.metaKey && !e.altKey) {
    const i = typeaheadIndex(
      all.map((el) => el.dataset.label ?? el.textContent ?? ""),
      at,
      e.key,
    );
    if (i >= 0) {
      e.preventDefault();
      all[i].focus();
    }
  }
}

/** Combobox keys (filter mode): ↑/↓ move the active option, Enter applies it, Esc closes. */
function onFilterKey(e: KeyboardEvent) {
  const n = options.value.length;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    if (n === 0) return;
    active.value = e.key === "ArrowDown" ? (active.value + 1) % n : (active.value - 1 + n) % n;
    void nextTick(() => popup.value?.querySelector(`#${CSS.escape(optionId(options.value[active.value]))}`)?.scrollIntoView({ block: "nearest" }));
  } else if (e.key === "Enter") {
    e.preventDefault();
    const v = options.value[active.value];
    if (v) choose(v);
  } else if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    if (filterText.value) filterText.value = "";
    else hide(true);
  }
}
function onFilterInput() {
  active.value = options.value.length > 0 ? 0 : -1;
}
function onPopupKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    hide(true);
  }
}

function choose(v: SavedView) {
  if (v.resolved.state === "unavailable") return;
  hide(true);
  emit("select", v);
}
function run(a: ActionItem) {
  hide(true);
  emit("action", a.key);
}
function retry() {
  emit("retry");
}
const isDefaultBadge = (v: SavedView) => v.isDefault && v.context === "inventory";
</script>

<template>
  <div ref="root" class="view-menu">
    <button
      ref="button"
      type="button"
      class="btn view-menu-button"
      :aria-haspopup="filterMode ? 'dialog' : 'menu'"
      :aria-expanded="open"
      :aria-controls="open ? ids.menu : undefined"
      @click="open ? hide(false) : show()"
      @keydown="onButtonKey"
    >
      <span class="muted">View:</span> <span class="view-menu-label" dir="auto">{{ label }}</span> <span aria-hidden="true">▾</span>
    </button>

    <!-- Many views: a filter (combobox) over a listbox of the views, then the actions as buttons. -->
    <div v-if="open && filterMode" :id="ids.menu" ref="popup" class="popover view-menu-popup" role="dialog" aria-label="Saved views" @keydown="onPopupKey">
      <label :for="ids.filter" class="sr-only">Filter views</label>
      <input
        :id="ids.filter"
        v-model="filterText"
        type="text"
        class="view-menu-filter"
        role="combobox"
        placeholder="Filter views…"
        autocomplete="off"
        aria-autocomplete="list"
        aria-expanded="true"
        :aria-controls="ids.list"
        :aria-activedescendant="active >= 0 && options[active] ? optionId(options[active]) : undefined"
        @input="onFilterInput"
        @keydown="onFilterKey"
      />
      <div :id="ids.list" role="listbox" class="view-menu-list" aria-label="Saved views">
        <template v-for="g in [{ key: 'mine', title: 'My views', id: ids.mine, views: groups.mine }, { key: 'shared', title: 'Shared views', id: ids.shared, views: groups.shared }]" :key="g.key">
          <div v-if="g.views.length > 0" role="group" :aria-labelledby="g.id">
            <div :id="g.id" role="presentation" class="view-menu-heading">{{ g.title }}</div>
            <div
              v-for="v in g.views"
              :id="optionId(v)"
              :key="v.id"
              role="option"
              :aria-selected="v.id === currentId"
              :aria-disabled="v.resolved.state === 'unavailable' || undefined"
              :title="v.resolved.state === 'unavailable' ? unavailableHint : v.description ?? undefined"
              :class="['view-menu-item', { active: options[active]?.id === v.id, disabled: v.resolved.state === 'unavailable' }]"
              @click="choose(v)"
            >
              <span class="check" aria-hidden="true">{{ v.id === currentId ? "✓" : "" }}</span>
              <span dir="auto">{{ v.name }}</span>
              <span v-if="isDefaultBadge(v)" class="badge spaced">Default</span>
              <span v-if="v.resolved.state === 'unavailable'" class="muted spaced">(unavailable)</span>
            </div>
          </div>
        </template>
        <p v-if="options.length === 0" class="muted view-menu-status">No view matches “{{ filterText }}”.</p>
      </div>
      <div class="view-menu-actions" role="group" aria-label="View actions">
        <button v-for="a in actionItems" :key="a.key" type="button" :class="['btn', 'btn-sm', { 'btn-danger': a.danger }]" @click="run(a)">{{ a.label }}</button>
      </div>
    </div>

    <!-- The menu: views as menuitemradio in two groups, then the actions. -->
    <div v-else-if="open" ref="popup" class="popover view-menu-popup" @keydown="onMenuKey">
      <p v-if="loading" class="view-menu-status" role="status"><span class="spinner" aria-hidden="true" /> Loading views…</p>
      <div v-else-if="error" class="view-menu-status alert alert-error" role="alert">
        Saved views could not be loaded.
        <span v-if="error.requestId" class="meta">Request ID: <span class="mono">{{ error.requestId }}</span></span>
      </div>
      <p v-else-if="(views?.length ?? 0) === 0" class="view-menu-status muted">
        No saved views yet. Set filters, sort and columns, then choose <strong>Save as new view</strong>.
      </p>
      <div :id="ids.menu" role="menu" aria-label="Saved views" class="view-menu-list">
        <button v-if="error" type="button" role="menuitem" tabindex="-1" data-menu-item data-label="Retry" class="view-menu-item" @click="retry">Retry</button>
        <template v-for="g in [{ key: 'mine', title: 'My views', id: ids.mine, views: groups.mine }, { key: 'shared', title: 'Shared views', id: ids.shared, views: groups.shared }]" :key="g.key">
          <template v-if="g.views.length > 0">
            <div :id="g.id" role="presentation" class="view-menu-heading">{{ g.title }}</div>
            <div role="group" :aria-labelledby="g.id">
              <button
                v-for="v in g.views"
                :key="v.id"
                type="button"
                role="menuitemradio"
                tabindex="-1"
                data-menu-item
                :data-label="v.name"
                :aria-checked="v.id === currentId"
                :aria-disabled="v.resolved.state === 'unavailable' || undefined"
                :aria-describedby="v.resolved.state === 'unavailable' ? `${ids.menu}-unavailable` : undefined"
                :title="v.resolved.state === 'unavailable' ? unavailableHint : v.description ?? undefined"
                :class="['view-menu-item', { disabled: v.resolved.state === 'unavailable' }]"
                @click="choose(v)"
              >
                <span class="check" aria-hidden="true">{{ v.id === currentId ? "✓" : "" }}</span>
                <span dir="auto">{{ v.name }}</span>
                <span v-if="isDefaultBadge(v)" class="badge spaced">Default</span>
                <span v-if="v.resolved.state === 'unavailable'" class="muted spaced">(unavailable)</span>
              </button>
            </div>
          </template>
        </template>
        <div role="separator" class="view-menu-separator" />
        <button
          v-for="a in actionItems"
          :key="a.key"
          type="button"
          role="menuitem"
          tabindex="-1"
          data-menu-item
          :data-label="a.label"
          :class="['view-menu-item', { danger: a.danger }]"
          @click="run(a)"
        >
          {{ a.label }}
        </button>
      </div>
      <span :id="`${ids.menu}-unavailable`" class="sr-only">{{ unavailableHint }}</span>
    </div>
  </div>
</template>
