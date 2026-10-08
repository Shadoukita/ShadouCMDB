<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, useId } from "vue";
import { RouterLink, type RouteLocationRaw } from "vue-router";
import Icon from "./Icon.vue";
import type { IconName } from "../icons/lucide";

export interface RowMenuItem {
  label: string;
  /** A link (Open, Impact analysis) … */
  to?: RouteLocationRaw;
  /** … or an action (Remove). */
  action?: () => void;
  danger?: boolean;
}

/**
 * A row's action menu (ARIA menu button): Enter, Space or Down opens it on the first item, Up on the last;
 * the arrow keys, Home and End move, Esc and Tab close it, and Esc returns focus to the button.
 */
const props = defineProps<{
  label: string;
  items: RowMenuItem[];
  /** At the control height, for a page header (default: small, for a table row). */
  large?: boolean;
  /**
   * The row's keyboard focus target (lib/rowKeyboard), for a row with no page of its own: ↑/↓ move between rows
   * instead of opening the menu, and Enter or Space opens it.
   */
  rowFocus?: boolean;
  /** A text button (a page header's Export) instead of the ellipsis icon: its text, and an icon before it. */
  text?: string;
  icon?: IconName;
  /**
   * The button stays focusable (aria-disabled, not disabled) so focus does not drop to the page when it turns disabled
   * under the keyboard, as after choosing an export format, and Tab still reaches it with its reason.
   */
  disabled?: boolean;
  /** Why it is disabled: the tooltip, and its accessible description for keyboard and screen-reader users. */
  title?: string;
}>();
const open = ref(false);
const button = ref<HTMLButtonElement>();
const menu = ref<HTMLElement>();
const menuId = `row-menu-${useId()}`;
const reasonId = `${menuId}-reason`;
/** The menu sits under <body> at the button's place: a table cell clips what overflows it. */
const place = ref<{ top: string; left: string }>({ top: "0", left: "0" });
/**
 * Puts the menu under the button, or above it when it does not fit below (the last rows of a page); false when the
 * button is out of view.
 */
function align(): boolean {
  const r = button.value?.getBoundingClientRect();
  if (!r) return false;
  const height = menu.value?.offsetHeight ?? 0;
  const above = r.bottom + 4 + height > window.innerHeight && r.top - 4 - height >= 0;
  place.value = { top: `${above ? r.top - 4 - height : r.bottom + 4}px`, left: `${Math.max(8, r.right - 200)}px` };
  return r.bottom > 0 && r.top < window.innerHeight;
}

const entries = () => [...(menu.value?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])];
async function show(at: "first" | "last" = "first") {
  align();
  open.value = true;
  document.addEventListener("pointerdown", onOutside, true);
  window.addEventListener("scroll", onScroll, true);
  await nextTick();
  // Rendered now: place it again with its height.
  align();
  const all = entries();
  (at === "first" ? all[0] : all[all.length - 1])?.focus();
}
function hide(refocus: boolean) {
  open.value = false;
  document.removeEventListener("pointerdown", onOutside, true);
  window.removeEventListener("scroll", onScroll, true);
  if (refocus) button.value?.focus();
}
function onOutside(e: Event) {
  const target = e.target as Node;
  if (!menu.value?.contains(target) && !button.value?.contains(target)) hide(false);
}
/**
 * A fixed menu would drift from its row when the page scrolls under it, so it follows the button, and closes once
 * the button has scrolled out of view. Closing on any scroll lost the menu to scroll events that arrive after the
 * click: the one of the scroll that brought the button into view, or a reflow from content loading above it.
 */
const onScroll = (e: Event) => {
  if (menu.value?.contains(e.target as Node)) return;
  if (!align()) hide(false);
};
onBeforeUnmount(() => hide(false));

function toggle() {
  if (open.value) hide(false);
  else if (!props.disabled) void show();
}
function onButtonKey(e: KeyboardEvent) {
  if (props.disabled) return;
  if ((e.key === "ArrowDown" || e.key === "ArrowUp") && !props.rowFocus) {
    e.preventDefault();
    void show(e.key === "ArrowDown" ? "first" : "last");
  }
}
function onMenuKey(e: KeyboardEvent) {
  const all = entries();
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
  }
}
function run(item: RowMenuItem) {
  hide(true);
  item.action?.();
}
</script>

<template>
  <div class="row-menu">
    <button
      ref="button"
      type="button"
      :class="['btn', { 'btn-icon': !text, 'btn-sm': !large }]"
      aria-haspopup="menu"
      :aria-expanded="open"
      :aria-controls="open ? menuId : undefined"
      :aria-label="text ? undefined : label"
      :aria-disabled="disabled || undefined"
      :aria-describedby="title ? reasonId : undefined"
      :title="title"
      :data-row-focus="rowFocus || undefined"
      @click="toggle"
      @keydown="onButtonKey"
    >
      <template v-if="text"><Icon v-if="icon" :name="icon" />{{ text }}<Icon name="chevron-down" /></template>
      <Icon v-else name="ellipsis" />
    </button>
    <span v-if="title" :id="reasonId" class="sr-only">{{ title }}</span>
    <Teleport to="body">
      <ul v-if="open" :id="menuId" ref="menu" class="row-menu-list" role="menu" :aria-label="label" :style="place" @keydown="onMenuKey">
        <li v-for="item in items" :key="item.label" role="none">
          <RouterLink v-if="item.to" role="menuitem" tabindex="-1" :to="item.to" @click="hide(false)">{{ item.label }}</RouterLink>
          <button v-else type="button" role="menuitem" tabindex="-1" :class="{ danger: item.danger }" @click="run(item)">{{ item.label }}</button>
        </li>
      </ul>
    </Teleport>
  </div>
</template>
