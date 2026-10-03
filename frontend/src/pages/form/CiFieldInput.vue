<script setup lang="ts">
import AttributeInput from "../../components/AttributeInput.vue";
import { nowFormValue, NOW_HINT } from "../../lib/attributeValues";
import { BUILTIN, cellClass } from "../../lib/uiSettings";
import type { CiDraft } from "./ciDraft";
import { FIELD_IDS } from "./ciDraft";
import FormField from "./FormField.vue";

/**
 * One field of a CI draft (ciDraft.ts) as an input with its label, hint and error: a core field or a class
 * attribute. A field the draft may not change is shown disabled. `width`/`columns` place it on the layout grid.
 */
defineProps<{ draft: CiDraft; f: string; width?: number; columns?: number }>();
</script>

<template>
  <FormField
    v-if="BUILTIN.has(f)"
    :id="FIELD_IDS[f]"
    v-slot="p"
    :class="width ? cellClass(width, columns) : undefined"
    :label="BUILTIN.get(f)!.label"
    :error="draft.coreError(f)"
    :hint="draft.coreHint(f)"
    :required="f === 'validFrom'"
  >
    <fieldset class="ro-wrap" :disabled="!draft.editable(f)">
      <input
        v-if="f === 'ident'"
        :id="p.id"
        v-model="draft.core.ident"
        type="text"
        class="mono"
        spellcheck="false"
        :placeholder="draft.base ? undefined : 'Generated'"
        :aria-invalid="p.invalid || undefined"
        :aria-describedby="p.describedBy"
      />
      <select
        v-else-if="f === 'criticality'"
        :id="p.id"
        v-model="draft.criticalityId"
        :disabled="draft.criticalityLoading"
        :aria-invalid="p.invalid || undefined"
        :aria-describedby="p.describedBy"
      >
        <option value="">{{ draft.criticalityLoading ? "Loading…" : draft.criticalityError ? "Could not load the list" : "— not set —" }}</option>
        <option v-for="v in draft.criticalityOptions" :key="v.id" :value="v.id">{{ v.name }}{{ v.isActive ? "" : " (retired)" }}</option>
        <option v-if="draft.criticalityStray" :value="draft.criticalityId">{{ draft.base?.criticality?.name ?? "Unknown value" }}</option>
      </select>
      <input
        v-else-if="f === 'validFrom' || f === 'validUntil'"
        :id="p.id"
        v-model="draft.core[f]"
        type="datetime-local"
        :title="NOW_HINT"
        :aria-invalid="p.invalid || undefined"
        :aria-describedby="p.describedBy"
        @dblclick="draft.core[f] = nowFormValue('datetime')"
      />
    </fieldset>
  </FormField>
  <FormField
    v-else-if="draft.defFor(f)"
    :id="`attr-${draft.defFor(f)!.key}`"
    v-slot="p"
    :class="width ? cellClass(width, columns) : undefined"
    :label="draft.defFor(f)!.label"
    :required="draft.defFor(f)!.isRequired"
    :error="draft.fieldErrors[f]"
    :hint="draft.attrHint(draft.defFor(f)!)"
  >
    <fieldset class="ro-wrap" :disabled="!draft.editable(f)">
      <AttributeInput
        :id="p.id"
        v-model="draft.values[draft.defFor(f)!.key]"
        :def="draft.defFor(f)!"
        :invalid="p.invalid"
        :described-by="p.describedBy"
        :reference-name="draft.refNames[draft.defFor(f)!.key]"
        :lookup-parent="draft.lookupParent(draft.defFor(f)!)"
        @reference-name="(name) => (draft.refNames[draft.defFor(f)!.key] = name)"
      />
    </fieldset>
  </FormField>
</template>
