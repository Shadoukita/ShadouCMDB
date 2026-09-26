import { ref, toValue, watch, watchEffect, type MaybeRefOrGetter, type Ref } from "vue";

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

export function useDocumentTitle(title: MaybeRefOrGetter<string | undefined>) {
  watchEffect(() => {
    const t = toValue(title);
    document.title = t ? `${t} · ShadouCMDB` : "ShadouCMDB";
  });
}
