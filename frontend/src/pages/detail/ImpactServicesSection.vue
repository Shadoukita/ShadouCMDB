<script setup lang="ts">
import { computed, ref, useId } from "vue";
import { useServicesById, useServiceSettings, type PrincipalRef } from "../../api/services";
import CiLink from "../../components/CiLink.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import { t } from "../../i18n";
import { edgeLabel, type ImpactAnalysis } from "../../lib/impact";
import { affectedServices } from "../../lib/serviceMembers";
import type { TrailStep } from "../../lib/trail";

/**
 * The Impact tab's pinned "Affected business services (N)" (spec SHAA-927 §2, §5.7): the result's items of the
 * business service class, most critical first, then the nearest. Computed here from `items[].classId` and the class
 * from /settings/business-services, so it needs no endpoint of its own. A truncated or depth-bounded result says
 * so in the header: the section is never presented as the complete list.
 */
const props = defineProps<{ analysis: ImpactAnalysis; self: TrailStep; trail: TrailStep[] }>();

/** Owners are looked up per service; past this many rows the service page has them. */
const OWNER_LOOKUPS = 50;

const settings = useServiceSettings();
const services = computed(() => (settings.data.value?.canView ? affectedServices(props.analysis.items, settings.data.value.classId) : []));
const details = useServicesById(() => services.value.slice(0, OWNER_LOOKUPS).map((s) => s.id));
const incomplete = computed(() => props.analysis.truncated || props.analysis.hasMoreBeyondDepth);

const byId = computed(() => new Map(props.analysis.items.map((i) => [i.id, i.name])));
const parentName = (id: string) => (id === props.analysis.root.id ? props.analysis.root.name : (byId.value.get(id) ?? ""));
const ownerText = (o: PrincipalRef) => (o.active ? o.displayName : `${o.displayName} ${t("services.owners.disabled")}`);

const open = ref(true);
const bodyId = `impact-services-${useId()}`;
</script>

<template>
  <section v-if="services.length > 0" class="impact-services" :aria-labelledby="`${bodyId}-title`">
    <h3 :id="`${bodyId}-title`" class="impact-services-title">
      <button type="button" class="group-toggle" :aria-expanded="open" :aria-controls="bodyId" @click="open = !open">
        <span aria-hidden="true">{{ open ? "▾" : "▸" }}</span> {{ t("impact.services.title", { n: services.length }) }}
      </button>
      <span v-if="incomplete" class="muted impact-services-caveat">· {{ t("impact.services.incomplete", { depth: analysis.parameters.depth }) }}</span>
    </h3>
    <div v-show="open" :id="bodyId" class="table-wrap">
      <table class="data">
        <caption class="sr-only">{{ t("impact.services.title", { n: services.length }) }}</caption>
        <thead>
          <tr>
            <th scope="col">{{ t("impact.services.col.service") }}</th>
            <th scope="col">{{ t("services.col.criticality") }}</th>
            <th scope="col">{{ t("services.col.technicalOwners") }}</th>
            <th scope="col" class="num">{{ t("impact.services.col.hops") }}</th>
            <th scope="col">{{ t("impact.services.col.via") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="s in services" :key="s.id">
            <td>
              <CiLink :id="s.id" service :from="self" :trail="trail">{{ s.name }}</CiLink>
              <span class="mono muted impact-ident">{{ s.ident }}</span>
            </td>
            <td><CriticalityBadge :value="s.criticality" show-unset /></td>
            <td>
              <template v-if="details.get(s.id)">
                <template v-if="details.get(s.id)!.owners.technical.length > 0">
                  <template v-for="(o, i) in details.get(s.id)!.owners.technical" :key="o.id"><template v-if="i > 0">, </template><bdi>{{ ownerText(o) }}</bdi></template>
                </template>
                <span v-else class="muted">{{ t("services.owners.noneShort") }}</span>
              </template>
              <span v-else class="muted" :title="t('impact.services.ownersNotLoaded')">—<span class="sr-only"> {{ t("impact.services.ownersNotLoaded") }}</span></span>
            </td>
            <td class="num">{{ s.hops }}</td>
            <td>
              <span class="muted"><bdi>{{ edgeLabel(s.via, s.id) }}</bdi></span>{{ " " }}
              <CiLink :id="s.via.parentId" service :from="self" :trail="trail">{{ parentName(s.via.parentId) }}</CiLink>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
