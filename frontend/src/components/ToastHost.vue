<script setup lang="ts">
import { t } from "../i18n";
import { useFlashStore } from "../stores/flash";
import Icon from "./Icon.vue";

/**
 * The toast stack in the bottom-right corner of the shell (design document §2.7, audit F1).
 * The live region is always in the page, so a screen reader announces each toast as it is added.
 * Focus inside the stack pauses the clocks (WCAG 2.2.1); the pointer does not, so a toast over a
 * button closes on time instead of waiting for the pointer to leave.
 */
const flash = useFlashStore();

function onFocusOut(e: FocusEvent) {
  if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node | null)) flash.resume();
}
</script>

<template>
  <div class="toast-host" role="status" @focusin="flash.pause()" @focusout="onFocusOut">
    <div v-for="toast in flash.toasts" :key="toast.id" class="toast alert alert-success">
      <span class="toast-text">{{ toast.text }}</span>
      <button
        type="button"
        class="btn btn-ghost btn-icon btn-sm toast-close"
        :aria-label="t('shell.toast.dismiss')"
        :title="t('shell.toast.dismiss')"
        @click="flash.dismiss(toast.id)"
      >
        <Icon name="x" />
      </button>
    </div>
  </div>
</template>
