<script setup lang="ts">
import { ref } from "vue";
import { t } from "../i18n";
import { useFlashStore } from "../stores/flash";
import Icon from "./Icon.vue";

/**
 * The toast stack in the bottom-right corner of the shell (design document §2.7, audit F1).
 * The live region is always in the page, so a screen reader announces each toast as it is added.
 * Focus inside the stack pauses the clocks (WCAG 2.2.1); the pointer does not, so a toast over a
 * button closes on time instead of waiting for the pointer to leave.
 * Dismissing the focused toast hands focus on before the toast leaves the page (WCAG 2.4.3): to the
 * next toast's close button, else back to where the operator was before entering the stack, else
 * `main`. Moving focus first lets `focusout` resume the clocks as usual.
 */
const flash = useFlashStore();
const host = ref<HTMLElement | null>(null);
/** The element that had focus when the operator entered the stack. */
let returnTo: HTMLElement | null = null;

function onFocusIn(e: FocusEvent) {
  const from = e.relatedTarget as HTMLElement | null;
  if (!host.value?.contains(from)) returnTo = from;
  flash.pause();
}

function onFocusOut(e: FocusEvent) {
  if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node | null)) flash.resume();
}

function focusTarget(id: number): HTMLElement | null {
  const toasts = flash.toasts;
  const i = toasts.findIndex((toast) => toast.id === id);
  const neighbour = toasts[i + 1] ?? toasts[i - 1];
  if (neighbour) {
    const button = host.value?.querySelector<HTMLElement>(`[data-toast-id="${neighbour.id}"] .toast-close`);
    if (button) return button;
  }
  if (returnTo?.isConnected) return returnTo;
  return document.getElementById("main");
}

function dismiss(id: number) {
  if (host.value?.contains(document.activeElement)) {
    const target = focusTarget(id);
    // `main` is not focusable by itself; tabindex -1 lets it take focus without joining the tab order.
    if (target && target.tabIndex < 0 && !target.hasAttribute("tabindex")) target.setAttribute("tabindex", "-1");
    target?.focus({ preventScroll: true });
  }
  flash.dismiss(id);
}
</script>

<template>
  <div ref="host" class="toast-host" role="status" @focusin="onFocusIn" @focusout="onFocusOut">
    <div
      v-for="toast in flash.toasts"
      :key="toast.id"
      class="toast alert alert-success"
      :data-toast-id="toast.id"
    >
      <span class="toast-text">{{ toast.text }}</span>
      <button
        type="button"
        class="btn btn-ghost btn-icon btn-sm toast-close"
        :aria-label="t('shell.toast.dismiss')"
        :title="t('shell.toast.dismiss')"
        @click="dismiss(toast.id)"
      >
        <Icon name="x" />
      </button>
    </div>
  </div>
</template>
