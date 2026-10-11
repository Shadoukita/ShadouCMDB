<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import type { AuditEntry, EffectiveAttribute } from "../../api/queries";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";
import ChangeValue from "../imports/ChangeValue.vue";

/**
 * A workflow step on the CI (`workflow.*`) in its history: the workflow and transition, the state it moved
 * from and to, the comment, and the fields it set as old → new chips. Its details are in the values, not a
 * field diff. `fieldDef`: the field's definition, so lookups and references show names.
 */
const props = defineProps<{ entry: AuditEntry; fieldDef: (key: string) => EffectiveAttribute | undefined }>();

type WorkflowValue = { instanceId?: string; definitionKey?: string; transitionKey?: string; stateKey?: string; comment?: string | null; reason?: string; fields?: Record<string, { old?: unknown; new?: unknown }> };
const w = computed(() => {
  const e = props.entry;
  const o = (e.oldValue ?? {}) as WorkflowValue;
  const n = (e.newValue ?? {}) as WorkflowValue;
  const show = (v: unknown) => (v === null || v === undefined ? undefined : typeof v === "object" ? JSON.stringify(v) : String(v));
  return {
    instanceId: n.instanceId ?? o.instanceId,
    definitionKey: n.definitionKey ?? o.definitionKey ?? "",
    transitionKey: n.transitionKey,
    from: e.action === "workflow.cancel" ? undefined : o.stateKey,
    to: n.stateKey,
    note: n.reason === "ci_deleted" ? t("history.wf.ciDeleted") : (n.comment ?? n.reason ?? undefined),
    fields: Object.entries(n.fields ?? {}).map(([k, v]) => ({ key: k, def: props.fieldDef(k), old: show(v?.old), new: show(v?.new) })),
  };
});
</script>

<template>
  <div class="event-change" data-testid="history-workflow">
    <RouterLink v-if="w.instanceId" :to="`/workflows/${w.instanceId}`" class="mono">{{ w.definitionKey }}</RouterLink>
    <span v-else class="mono">{{ w.definitionKey }}</span>
    <template v-if="w.transitionKey">: <code>{{ w.transitionKey }}</code></template>
    <span v-if="w.to">
      ,
      <template v-if="w.from">
        <span class="sr-only">{{ t("history.diff.from") }}</span><code>{{ w.from }}</code>
        <Icon name="arrow-right" :size="12" class="diff-arrow" />
      </template>
      <span class="sr-only">{{ t("history.diff.to") }}</span><code>{{ w.to }}</code>
    </span>
    <div v-if="w.note" class="wf-comment muted" dir="auto">{{ w.note }}</div>
    <ul v-if="w.fields.length > 0" class="diff">
      <li v-for="c in w.fields" :key="c.key">
        <code>{{ c.key }}</code>
        <span class="sr-only">{{ t("history.diff.from") }}</span>
        <del v-if="c.old !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del><span v-else class="diff-none" aria-hidden="true">—</span>
        <Icon name="arrow-right" :size="12" class="diff-arrow" />
        <span class="sr-only">{{ t("history.diff.to") }}</span>
        <ins v-if="c.new !== undefined" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">{{ t("history.diff.cleared") }}</span>
      </li>
    </ul>
  </div>
</template>
