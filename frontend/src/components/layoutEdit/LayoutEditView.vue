<script setup lang="ts">
import type { EffectiveAttribute } from "../../api/queries";
import type { LayoutEditor } from "../../lib/layoutEditor";
import type { PanelKind } from "../../lib/uiSettings";
import { t } from "../../i18n";
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
  panel?(p: { kind: PanelKind }): unknown;
}>();
</script>

<template>
  <LoadingState v-if="editor.loading" :label="t('layoutEditor.loading')" />
  <ErrorAlert v-else-if="editor.loadError" :error="editor.loadError" :title="t('layoutEditor.loadFailed')" :on-retry="() => editor.reload()" />
  <ErrorAlert v-else-if="attrsError" :error="attrsError" :title="t('layoutEditor.attrsFailed')" />
  <LoadingState v-else-if="!attrs || !editor.layout" :label="t('record.loadingAttrs')" />
  <template v-else>
    <LayoutEditBar :editor="editor" :class-name="className" />
    <LayoutCanvas :editor="editor" :attrs="attrs" :form="form">
      <template #field="{ field }"><slot name="field" :field="field" /></template>
      <template v-if="$slots.panel" #panel="{ kind }"><slot name="panel" :kind="kind" /></template>
    </LayoutCanvas>
  </template>
</template>
