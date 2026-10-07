import { onBeforeUnmount, ref, toValue, watch, watchEffect, type MaybeRefOrGetter, type Ref } from "vue";
import { useBrandingStore } from "../stores/branding";
import { onBeforeRouteLeave, onBeforeRouteUpdate } from "vue-router";

export function useDebounced<T>(source: MaybeRefOrGetter<T>, ms = 250): Ref<T> {
  const v = ref(toValue(source)) as Ref<T>;
  let t: ReturnType<typeof setTimeout> | undefined;
  watch(
    () => toValue(source),
    (next) => {
      clearTimeout(t);
      t = setTimeout(() => (v.value = next), ms);
    },
  );
  return v;
}

/** "<page> · <app name>"; the app name comes from Customization › Branding. */
export function useDocumentTitle(title: MaybeRefOrGetter<string | undefined>) {
  const branding = useBrandingStore();
  watchEffect(() => {
    const t = toValue(title);
    const app = branding.effective.appName;
    document.title = t ? `${t} · ${app}` : app;
  });
}

/** Whether a CSS media query matches, kept live as the window resizes. */
export function useMediaQuery(query: string): Ref<boolean> {
  const mql = window.matchMedia(query);
  const matches = ref(mql.matches);
  const onChange = (e: MediaQueryListEvent) => (matches.value = e.matches);
  mql.addEventListener("change", onChange);
  onBeforeUnmount(() => mql.removeEventListener("change", onChange));
  return matches;
}

/**
 * Unsaved changes on an edit page: confirm before leaving it in the app (another route, or the same page for
 * another record), and let the browser ask before a reload or closing the tab. `allow()` lets the next
 * navigation through without asking, for the page's own redirect after a create or a delete.
 */
export function useUnsavedGuard(dirty: () => boolean, message: () => string): { allow: () => void } {
  let allowed = false;
  const keep = () => {
    if (allowed) {
      allowed = false;
      return true;
    }
    return !dirty() || window.confirm(message());
  };
  onBeforeRouteLeave(keep);
  onBeforeRouteUpdate((to, from) => to.path === from.path || keep());
  const onBeforeUnload = (e: BeforeUnloadEvent) => {
    if (!dirty()) return;
    e.preventDefault();
    e.returnValue = "";
  };
  window.addEventListener("beforeunload", onBeforeUnload);
  onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));
  return { allow: () => (allowed = true) };
}
