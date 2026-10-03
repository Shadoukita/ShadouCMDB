import { defineStore } from "pinia";
import { ref } from "vue";

export interface Toast {
  id: number;
  text: string;
}

/** How long a toast stays on screen (design document §2.7). */
export const TOAST_MS = 6000;
/** At most this many toasts are on screen; a new one pushes out the oldest. */
export const TOAST_MAX = 4;

/**
 * One-shot confirmations ("Created crm-app-01.") shown as toasts by ToastHost in the shell.
 * They outlive the navigation that usually follows a create or delete, so a message no longer needs
 * a key for the page that should show it, and nothing on the page moves when it closes. Errors are
 * not toasts: they stay inline next to what failed (ErrorAlert, field errors).
 * While keyboard focus is on a toast, `pause()` stops the clocks so it never closes under the reader;
 * `resume()` gives each toast its full time again.
 */
export const useFlashStore = defineStore("flash", () => {
  const toasts = ref<Toast[]>([]);
  const timers = new Map<number, ReturnType<typeof setTimeout>>();
  let nextId = 1;
  let paused = false;

  function arm(t: Toast) {
    if (paused) return;
    clearTimeout(timers.get(t.id));
    timers.set(t.id, setTimeout(() => dismiss(t.id), TOAST_MS));
  }

  function show(text: string): number {
    const t: Toast = { id: nextId++, text };
    toasts.value = [...toasts.value, t];
    while (toasts.value.length > TOAST_MAX) dismiss(toasts.value[0].id);
    arm(t);
    return t.id;
  }

  function dismiss(id: number) {
    clearTimeout(timers.get(id));
    timers.delete(id);
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }

  function pause() {
    paused = true;
    for (const timer of timers.values()) clearTimeout(timer);
    timers.clear();
  }

  function resume() {
    paused = false;
    toasts.value.forEach(arm);
  }

  return { toasts, show, dismiss, pause, resume };
});
