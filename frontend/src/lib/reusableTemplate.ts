import { defineComponent, shallowRef, type Slot, type SlotsType, type VNodeChild } from "vue";

/**
 * A piece of a component's template defined once and rendered in several places
 * (the CI form's field, in the form and in layout edit mode):
 *
 *   const [DefineField, Field] = createReusableTemplate<{ f: string }>();
 *   <DefineField v-slot="{ f }">…</DefineField>   (renders nothing itself)
 *   <Field :f="…" />                              (renders the piece)
 *
 * `Define` must come before the first `Field` in the template.
 */
export function createReusableTemplate<Props extends Record<string, unknown>>() {
  const render = shallowRef<Slot | undefined>();
  const Define = defineComponent({
    slots: Object as SlotsType<{ default: (props: Props) => VNodeChild }>,
    setup(_, { slots }) {
      return () => {
        render.value = slots.default as Slot | undefined;
        return null;
      };
    },
  });
  const Reuse = defineComponent({
    inheritAttrs: false,
    setup(_, { attrs }) {
      return () => render.value?.(attrs as Props);
    },
  });
  return [Define, Reuse as unknown as new () => { $props: Props }] as const;
}
