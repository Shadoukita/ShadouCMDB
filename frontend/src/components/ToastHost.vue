<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { t } from "../i18n";
import { useFlashStore, type Toast } from "../stores/flash";
import Icon from "./Icon.vue";

/**
 * The shell's toast stack (SHAA-1670 design document §2.7, audit F1), bottom right, fed by
 * stores/flash.ts. Both live regions are always in the DOM, so screen readers announce what is
 * added to them: errors in the assertive one, as `role=alert`, confirmations in the polite one, as
 * `role=status`. The empty regions carry no role, so they never match a role query.
 * F8 moves focus to the newest toast; Escape or its close button dismisses it, and focus returns
 * to where the operator was.
 */
const flash = useFlashStore();
const errors = computed(() => flash.toasts.filter((x) => x.tone === "danger"));
const notes = computed(() => flash.toasts.filter((x) => x.tone !== "danger"));

const host = ref<HTMLElement | null>(null);
// Where focus was before it entered the stack (F8, Tab): it goes back there when the stack empties.
let returnTo: HTMLElement | null = null;

function closeButtons(): HTMLButtonElement[] {
  return [...(host.value?.querySelectorAll<HTMLButtonElement>(".toast-close") ?? [])];
}

function onGlobalKey(e: KeyboardEvent) {
  if (e.key !== "F8" || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
  const buttons = closeButtons();
  if (!buttons.length) return;
  e.preventDefault();
  // The newest toast: errors are listed first, confirmations after them, each oldest first.
  buttons[buttons.length - 1].focus();
}
onMounted(() => window.addEventListener("keydown", onGlobalKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onGlobalKey));

function onFocusIn(e: FocusEvent) {
  const from = e.relatedTarget as HTMLElement | null;
  if (from && !host.value?.contains(from)) returnTo = from;
}

async function dismiss(toast: Toast) {
  const hadFocus = !!host.value?.contains(document.activeElement);
  const buttons = closeButtons();
  const index = buttons.findIndex((b) => b.dataset.toastId === String(toast.id));
  flash.dismiss(toast.id);
  if (!hadFocus) return;
  await nextTick();
  const rest = closeButtons();
  const next = rest[Math.min(index, rest.length - 1)];
  if (next) next.focus();
  else if (returnTo?.isConnected) returnTo.focus();
  else document.getElementById("main")?.focus();
}
</script>

<template>
  <section ref="host" class="toast-host" :aria-label="t('toast.region', { key: 'F8' })" @focusin="onFocusIn">
    <div aria-live="assertive" aria-relevant="additions" class="toast-list">
      <div
        v-for="x in errors"
        :key="x.id"
        class="toast toast-danger"
        role="alert"
        data-testid="toast"
        @keydown.esc.stop="dismiss(x)"
      >
        <Icon name="circle-alert" class="toast-icon" />
        <p class="toast-text">{{ x.text }}</p>
        <button type="button" class="btn btn-ghost btn-sm btn-icon toast-close" :data-toast-id="x.id" :aria-label="t('toast.dismiss')" :title="t('toast.dismiss')" @click="dismiss(x)">
          <Icon name="x" />
        </button>
      </div>
    </div>
    <div aria-live="polite" aria-relevant="additions" class="toast-list">
      <div
        v-for="x in notes"
        :key="x.id"
        class="toast toast-success"
        role="status"
        data-testid="toast"
        @mouseenter="flash.pause(x.id)"
        @mouseleave="flash.resume(x.id)"
        @focusin="flash.pause(x.id)"
        @focusout="flash.resume(x.id)"
        @keydown.esc.stop="dismiss(x)"
      >
        <Icon name="circle-check" class="toast-icon" />
        <p class="toast-text">{{ x.text }}</p>
        <button type="button" class="btn btn-ghost btn-sm btn-icon toast-close" :data-toast-id="x.id" :aria-label="t('toast.dismiss')" :title="t('toast.dismiss')" @click="dismiss(x)">
          <Icon name="x" />
        </button>
      </div>
    </div>
  </section>
</template>
