<script setup lang="ts">
import type { EffectiveAttribute } from "../../api/queries";
import type { LayoutEditor } from "../../lib/layoutEditor";
import type { PanelKind } from "../../lib/uiSettings";
import ErrorAlert from "../ErrorAlert.vue";
import LoadingState from "../LoadingState.vue";
import LayoutCanvas from "./LayoutCanvas.vue";
import LayoutEditBar from "./LayoutEditBar.vue";

/** A CI page in layout edit mode: the edit bar over the page's fields as an editable canvas. */
defineProps<{
  editor: LayoutEditor;
  className: string;
  /** The class's active attributes; undefined while they load. */
  attrs: readonly EffectiveAttribute[] | undefined;
  attrsError?: unknown;
  form?: boolean;
}>();
defineSlots<{
  field(p: { field: string }): unknown;
  "first-tab-end"(): unknown;
  panel?(p: { kind: PanelKind }): unknown;
}>();
</script>

<template>
  <LoadingState v-if="editor.loading" label="Loading the layout…" />
  <ErrorAlert v-else-if="editor.loadError" :error="editor.loadError" title="Could not load the layout for editing" :on-retry="() => editor.reload()" />
  <ErrorAlert v-else-if="attrsError" :error="attrsError" title="Could not load the class's attributes" />
  <LoadingState v-else-if="!attrs || !editor.layout" label="Loading attribute definitions…" />
  <template v-else>
    <LayoutEditBar :editor="editor" :class-name="className" />
    <LayoutCanvas :editor="editor" :attrs="attrs" :form="form">
      <template #field="{ field }"><slot name="field" :field="field" /></template>
      <template #first-tab-end><slot name="first-tab-end" /></template>
      <template v-if="$slots.panel" #panel="{ kind }"><slot name="panel" :kind="kind" /></template>
    </LayoutCanvas>
  </template>
</template>
