import { onBeforeUnmount, ref, toValue, watch, watchEffect, type MaybeRefOrGetter, type Ref } from "vue";
import { useBrandingStore } from "../stores/branding";

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
