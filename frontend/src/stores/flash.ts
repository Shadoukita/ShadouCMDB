import { defineStore } from "pinia";
import { ref } from "vue";

/**
 * Confirmations and failures shown as toasts by the shell's ToastHost (SHAA-1670 design document
 * §2.7, audit F1). A page reports "Created crm-app-01." and may navigate away straight after: the
 * toast belongs to the shell, so it survives the route change, never pushes page content down, and
 * needs no record id to know where it belongs.
 *
 * Success toasts close after SUCCESS_TTL_MS; the timer pauses while the pointer or focus is on the
 * toast. Error toasts stay until dismissed. At most MAX_TOASTS are shown: the oldest success makes
 * room first, an error is never dropped unread.
 */
export type ToastTone = "success" | "danger";
export interface Toast {
  id: number;
  tone: ToastTone;
  text: string;
}

export const SUCCESS_TTL_MS = 6000;
export const MAX_TOASTS = 4;

export const useFlashStore = defineStore("flash", () => {
  const toasts = ref<Toast[]>([]);
  let nextId = 1;
  // Per success toast: the running timer, or the time left while paused.
  const timers = new Map<number, { handle?: ReturnType<typeof setTimeout>; deadline: number; left: number }>();

  function arm(id: number, ms: number) {
    const handle = setTimeout(() => dismiss(id), ms);
    timers.set(id, { handle, deadline: Date.now() + ms, left: ms });
  }

  function push(tone: ToastTone, text: string): number {
    // The same message again (Save pressed twice) restarts the existing toast instead of stacking a copy.
    const same = toasts.value.find((x) => x.tone === tone && x.text === text);
    if (same) {
      if (tone === "success") {
        clearTimeout(timers.get(same.id)?.handle);
        arm(same.id, SUCCESS_TTL_MS);
      }
      return same.id;
    }
    const id = nextId++;
    toasts.value = [...toasts.value, { id, tone, text }];
    if (tone === "success") arm(id, SUCCESS_TTL_MS);
    while (toasts.value.length > MAX_TOASTS) {
      const oldest = toasts.value.find((x) => x.tone === "success" && x.id !== id) ?? toasts.value.find((x) => x.tone === "success");
      if (!oldest) break;
      dismiss(oldest.id);
    }
    return id;
  }

  /** A change was saved: closes by itself. */
  function success(text: string): number {
    return push("success", text);
  }

  /** Something failed that has no field or panel to show it in: stays until dismissed. */
  function error(text: string): number {
    return push("danger", text);
  }

  function dismiss(id: number) {
    clearTimeout(timers.get(id)?.handle);
    timers.delete(id);
    toasts.value = toasts.value.filter((x) => x.id !== id);
  }

  /** Stops a success toast's countdown while the operator is reading or about to dismiss it. */
  function pause(id: number) {
    const t = timers.get(id);
    if (!t?.handle) return;
    clearTimeout(t.handle);
    timers.set(id, { deadline: t.deadline, left: Math.max(0, t.deadline - Date.now()) });
  }

  /** Restarts the countdown with the time that was left, but never less than two seconds. */
  function resume(id: number) {
    const t = timers.get(id);
    if (!t || t.handle) return;
    arm(id, Math.max(t.left, 2000));
  }

  function clear() {
    for (const t of timers.values()) clearTimeout(t.handle);
    timers.clear();
    toasts.value = [];
  }

  return { toasts, success, error, dismiss, pause, resume, clear };
});
