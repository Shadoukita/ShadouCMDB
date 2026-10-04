<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, useId } from "vue";
import { RouterLink, type RouteLocationRaw } from "vue-router";
import Icon from "./Icon.vue";

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
defineProps<{ label: string; items: RowMenuItem[]; /** At the control height, for a page header (default: small, for a table row). */ large?: boolean }>();
const open = ref(false);
const button = ref<HTMLButtonElement>();
const menu = ref<HTMLElement>();
const menuId = `row-menu-${useId()}`;
/** The menu sits under <body> at the button's place: a table cell clips what overflows it. */
const place = ref<{ top: string; left: string }>({ top: "0", left: "0" });
/** Where the button was when the menu opened. */
let anchor = { top: 0, right: 0 };

const entries = () => [...(menu.value?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])];
async function show(at: "first" | "last" = "first") {
  const r = button.value?.getBoundingClientRect();
  if (r) {
    anchor = { top: r.top, right: r.right };
    place.value = { top: `${r.bottom + 4}px`, left: `${Math.max(8, r.right - 200)}px` };
  }
  open.value = true;
  document.addEventListener("pointerdown", onOutside, true);
  window.addEventListener("scroll", onScroll, true);
  await nextTick();
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
 * A fixed menu would drift from its row when the page scrolls under it. A scroll that left the button in place
 * is ignored: the scroll event of the scroll that brought the button into view can arrive after the click.
 */
const onScroll = (e: Event) => {
  if (menu.value?.contains(e.target as Node)) return;
  const r = button.value?.getBoundingClientRect();
  if (!r || Math.abs(r.top - anchor.top) > 1 || Math.abs(r.right - anchor.right) > 1) hide(false);
};
onBeforeUnmount(() => hide(false));

function onButtonKey(e: KeyboardEvent) {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
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
      :class="['btn', 'btn-icon', { 'btn-sm': !large }]"
      aria-haspopup="menu"
      :aria-expanded="open"
      :aria-controls="open ? menuId : undefined"
      :aria-label="label"
      @click="open ? hide(false) : show()"
      @keydown="onButtonKey"
    >
      <Icon name="ellipsis" />
    </button>
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
