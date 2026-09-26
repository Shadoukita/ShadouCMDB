<script setup lang="ts">
import { RouterLink } from "vue-router";
import type { Ci } from "../../api/queries";
import { formatDateTime } from "../../lib/format";

defineProps<{ ci: Ci }>();
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>General</h2></div>
    <div class="panel-body">
      <dl class="props">
        <dt>Class</dt>
        <dd><RouterLink :to="`/cis?classId=${ci.classId}`">{{ ci.class.name }}</RouterLink></dd>
        <dt>Status</dt>
        <dd><RouterLink :to="`/cis?statusId=${ci.statusId}`">{{ ci.status.name }}</RouterLink></dd>
        <dt>Environment</dt>
        <dd>
          <RouterLink v-if="ci.environment" :to="`/cis?environmentId=${ci.environment.id}`">{{ ci.environment.name }}</RouterLink>
          <span v-else class="muted">—</span>
        </dd>
        <dt>Owner</dt>
        <dd>
          <RouterLink v-if="ci.owner" :to="`/cis?ownerId=${ci.owner.id}`">{{ ci.owner.name }}</RouterLink>
          <span v-else class="muted">—</span>
        </dd>
        <dt>Location</dt>
        <dd>
          <RouterLink v-if="ci.location" :to="`/cis?locationId=${ci.location.id}`" title="All CIs at this location">{{ ci.location.name }}</RouterLink>
          <span v-else class="muted">—</span>
        </dd>
        <dt>Hostname</dt>
        <dd><span v-if="ci.hostname" class="mono">{{ ci.hostname }}</span><span v-else class="muted">—</span></dd>
        <dt>IP address</dt>
        <dd><span v-if="ci.ipAddress" class="mono">{{ ci.ipAddress }}</span><span v-else class="muted">—</span></dd>
        <dt>Serial number</dt>
        <dd><span v-if="ci.serialNumber" class="mono">{{ ci.serialNumber }}</span><span v-else class="muted">—</span></dd>
        <dt>Notes</dt>
        <dd style="white-space: pre-wrap"><template v-if="ci.notes">{{ ci.notes }}</template><span v-else class="muted">—</span></dd>
        <dt>Created</dt>
        <dd>{{ formatDateTime(ci.createdAt) }}</dd>
        <dt>Updated</dt>
        <dd>{{ formatDateTime(ci.updatedAt) }} <span class="muted">· version {{ ci.version }}</span></dd>
        <dt>ID</dt>
        <dd class="mono">{{ ci.id }}</dd>
      </dl>
    </div>
  </section>
</template>
