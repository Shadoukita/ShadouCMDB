import type { Directive } from "vue";

/** Focus the element when it mounts (the `autofocus` attribute only applies on initial page load). */
export const vAutofocus: Directive<HTMLElement, boolean | undefined> = {
  mounted(el, binding) {
    if (binding.value !== false) el.focus();
  },
};
