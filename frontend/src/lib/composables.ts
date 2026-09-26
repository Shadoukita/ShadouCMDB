import { ref, toValue, watch, watchEffect, type MaybeRefOrGetter, type Ref } from "vue";
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
