<script setup lang="ts">
import type { AttributeReference, EffectiveAttribute } from "../../api/queries";
import { t } from "../../i18n";
import type { FieldKey, FieldValue } from "../../lib/ciHistory";
import { formatDateTime } from "../../lib/format";
import type { TrailStep } from "../../lib/trail";
import ChangeValue from "../imports/ChangeValue.vue";
import AttributeValue from "./AttributeValue.vue";

/**
 * One field's value at a past moment (SHAA-3131). A hidden or unrecorded value says so and is never shown.
 * An attribute renders as on the detail page; a reference recorded without its target (a workflow action's
 * entry) is named through the API, which withholds a CI the reader may not view.
 */
defineProps<{ field: FieldKey; value: FieldValue; def?: EffectiveAttribute; refInfo?: AttributeReference; self: TrailStep }>();
</script>

<template>
  <span v-if="value.kind === 'hidden'" class="muted" :title="t('history.pit.hiddenTitle')">{{ t("history.pit.hidden") }}</span>
  <span v-else-if="value.kind === 'notRecorded'" class="muted" :title="t('history.pit.notRecordedTitle')">{{ t("history.pit.notRecorded") }}</span>
  <span v-else-if="value.kind === 'unset'" class="muted">—<span class="sr-only">{{ t("history.pit.unset") }}</span></span>
  <template v-else-if="field === 'class' || field === 'criticality'"><bdi>{{ value.name ?? value.value }}</bdi></template>
  <template v-else-if="field === 'validFrom' || field === 'validUntil'">{{ formatDateTime(String(value.value)) }}</template>
  <span v-else-if="field === 'ident'" class="mono">{{ value.value }}</span>
  <AttributeValue v-else-if="def && (def.dataType !== 'reference' || refInfo)" :def="def" :value="value.value" :ref-info="refInfo" :self="self" :trail="[]" />
  <ChangeValue v-else :def="def" :value="value.value" />
</template>
