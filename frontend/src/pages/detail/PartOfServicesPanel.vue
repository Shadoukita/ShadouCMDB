<script setup lang="ts">
import { computed } from "vue";
import { useServiceSettings, useServicesOfCi, type PrincipalRef } from "../../api/services";
import CiLink from "../../components/CiLink.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import type { TrailStep } from "../../lib/trail";

/**
 * "Part of business services" on a CI's Overview (spec SHAA-927 §5.6): the services the CI is a member of, directly
 * or through nested services ("via Shop › Retail"). Only the heading shows while it loads; nothing at all when the
 * CI is part of none, or when the caller may not view business services (the API then answers an empty list).
 */
const props = defineProps<{ ci: { id: string; deletedAt?: string | null }; self: TrailStep; trail: TrailStep[] }>();

const settings = useServiceSettings();
const canView = computed(() => !!settings.data.value?.canView && !props.ci.deletedAt);
const services = useServicesOfCi(() => props.ci.id, canView);
const entries = computed(() => services.data.value?.data ?? []);
const nameOf = computed(() => new Map(entries.value.map((e) => [e.service.id, e.service.name])));
const shown = computed(() => canView.value && (services.isPending.value || services.isError.value || entries.value.length > 0));

/** "via {service}" around the chain of links: the text before and after the placeholder, in any language. */
const viaParts = computed(() => {
  const [before = "", after = ""] = t("services.partOf.via", { service: "\u0000" }).split("\u0000");
  return { before, after };
});
const ownerText = (o: PrincipalRef) => (o.active ? o.displayName : `${o.displayName} ${t("services.owners.disabled")}`);
</script>

<template>
  <section v-if="shown" class="panel part-of-services" aria-labelledby="part-of-title" :aria-busy="services.isPending.value">
    <div class="panel-header"><h2 id="part-of-title">{{ t("services.partOf.title") }}</h2></div>
    <div v-if="services.isError.value" class="panel-body">
      <ErrorAlert :error="services.error.value" :title="t('services.partOf.failed')" :on-retry="() => services.refetch()" />
    </div>
    <template v-else-if="entries.length > 0">
      <p v-if="services.data.value?.truncated" class="panel-body hint" role="note">{{ t("services.partOf.truncated") }}</p>
      <div class="table-wrap">
        <table class="data">
          <caption class="sr-only">{{ t("services.partOf.title") }}</caption>
          <thead>
            <tr>
              <th scope="col">{{ t("impact.services.col.service") }}</th>
              <th scope="col">{{ t("services.col.criticality") }}</th>
              <th scope="col">{{ t("services.col.technicalOwners") }}</th>
              <th scope="col">{{ t("services.partOf.membership") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="e in entries" :key="e.service.id">
              <td><CiLink :id="e.service.id" service :from="self" :trail="trail">{{ e.service.name }}</CiLink></td>
              <td><CriticalityBadge :value="e.service.criticality" show-unset /></td>
              <td>
                <template v-if="e.service.owners.technical.length > 0">
                  <template v-for="(o, i) in e.service.owners.technical" :key="o.id"><template v-if="i > 0">, </template><bdi>{{ ownerText(o) }}</bdi></template>
                </template>
                <span v-else class="muted">{{ t("services.owners.noneShort") }}</span>
              </td>
              <td>
                <template v-if="e.direct">{{ t("services.partOf.direct") }}</template>
                <span v-else class="part-of-chain">
                  {{ viaParts.before }}<template v-for="(id, i) in e.viaServiceIds" :key="id"><span v-if="i > 0" aria-hidden="true"> › </span><CiLink :id="id" service :from="self" :trail="trail">{{ nameOf.get(id) ?? id }}</CiLink></template>{{ viaParts.after }}
                </span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </section>
</template>
