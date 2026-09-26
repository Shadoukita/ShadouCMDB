<script setup lang="ts">
import type { Ci } from "../../api/queries";
import { BUILTIN, DETAIL_BUILTINS } from "../../lib/uiSettings";
import CoreFieldValue from "./CoreFieldValue.vue";

defineProps<{ ci: Ci }>();
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>General</h2></div>
    <div class="panel-body">
      <dl class="props">
        <template v-for="f in DETAIL_BUILTINS" :key="f">
          <dt>{{ BUILTIN.get(f)!.label }}</dt>
          <dd><CoreFieldValue :ci="ci" :field="f" /></dd>
        </template>
        <dt>ID</dt>
        <dd class="mono">{{ ci.id }}</dd>
      </dl>
    </div>
  </section>
</template>
