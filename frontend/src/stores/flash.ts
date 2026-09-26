import { defineStore } from "pinia";
import { ref } from "vue";

/**
 * One-shot confirmation shown on the next CI detail page ("Created crm-app-01.").
 * Tied to the CI id so it never leaks onto another record, and cleared after a
 * few seconds so reload/back do not repeat it.
 */
export const useFlashStore = defineStore("flash", () => {
  const message = ref<{ ciId: string; text: string } | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;

  function show(ciId: string, text: string) {
    clearTimeout(timer);
    message.value = { ciId, text };
    timer = setTimeout(() => (message.value = null), 6000);
  }

  function forCi(ciId: string): string | undefined {
    return message.value?.ciId === ciId ? message.value.text : undefined;
  }

  return { message, show, forCi };
});
