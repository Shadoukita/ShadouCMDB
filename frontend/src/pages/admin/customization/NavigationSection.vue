<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useAreas } from "../../../api/datamodel";
import { useCiClasses } from "../../../api/queries";
import type { UiNavEntry, UiSettingsDocument } from "../../../api/uiSettings";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { suggestKey } from "../../../lib/keys";
import { moveItem } from "../../../lib/reorder";
import { completeNavEntries, pageLabel } from "../../../lib/uiSettings";
import Icon from "../../../components/Icon.vue";

/**
 * Customization › Navigation: the main menu's order, names, sections and
 * hidden entries. The table always shows the complete menu (pages and classes
 * the settings do not mention yet included); the first change stores it whole.
 * Classes outside a section appear under their area's tab (Administration ›
 * Areas); the order here decides their order within the tab. The real menu on
 * the left previews the draft.
 */
const props = defineProps<{ doc: UiSettingsDocument }>();
const classes = useCiClasses();
const areas = useAreas();
const rows = computed(() => completeNavEntries(props.doc.navigation.entries, classes.data.value ?? [], areas.data.value ?? []));
const sections = computed(() => rows.value.filter((e) => e.type === "section"));

/** Applies a change to a copy of the complete menu and stores it. */
function edit(change: (entries: UiNavEntry[]) => void) {
  const next = completeNavEntries(props.doc.navigation.entries, classes.data.value ?? [], areas.data.value ?? []);
  change(next);
  props.doc.navigation.entries = next;
}

function className(key: string | undefined): string {
  const c = classes.data.value?.find((k) => k.key === key);
  if (!c) return `${key} (no such class)`;
  return `${c.name}${c.isActive ? "" : " (archived)"}${c.isAbstract ? " (incl. subclasses)" : ""}`;
}
function areaName(key: string | undefined): string {
  const c = classes.data.value?.find((k) => k.key === key);
  const a = c && areas.data.value?.find((x) => x.id === c.areaId);
  return a ? `${a.name}${a.isActive ? "" : " (archived)"}` : "";
}
function defaultName(e: UiNavEntry): string {
  if (e.type === "page") return pageLabel(e.page!);
  if (e.type === "class") return className(e.classKey);
  return e.key ?? "";
}
const kindLabel = (e: UiNavEntry) => (e.type === "page" ? "Page" : e.type === "class" ? "Class" : "Section");
const rowId = (e: UiNavEntry) => `${e.type}:${e.page ?? e.classKey ?? e.key}`;

const setLabel = (i: number, v: string) => edit((n) => (n[i].label = v.trim() ? v : null));
const setShown = (i: number, shown: boolean) => edit((n) => (n[i].hidden = !shown));
const move = (i: number, to: number) => edit((n) => n.splice(0, n.length, ...moveItem(n, i, to)));

function moveToSection(i: number, sectionKey: string) {
  if (!sectionKey) return;
  edit((n) => {
    const [entry] = n.splice(i, 1);
    const section = n.find((e) => e.type === "section" && e.key === sectionKey)!;
    section.items = [...(section.items ?? []), { classKey: entry.classKey!, label: entry.label ?? null, hidden: !!entry.hidden }];
  });
}
function removeSection(i: number) {
  edit((n) => {
    const s = n[i];
    const back: UiNavEntry[] = (s.items ?? []).map((it) => ({ type: "class", classKey: it.classKey, label: it.label ?? null, hidden: !!it.hidden }));
    n.splice(i, 1, ...back);
  });
}
const setItemLabel = (i: number, j: number, v: string) => edit((n) => (n[i].items![j].label = v.trim() ? v : null));
const setItemShown = (i: number, j: number, shown: boolean) => edit((n) => (n[i].items![j].hidden = !shown));
const moveItemIn = (i: number, j: number, to: number) => edit((n) => (n[i].items = moveItem(n[i].items!, j, to)));
function moveOut(i: number, j: number) {
  edit((n) => {
    const [it] = n[i].items!.splice(j, 1);
    n.splice(i + 1, 0, { type: "class", classKey: it.classKey, label: it.label ?? null, hidden: !!it.hidden });
  });
}

const newSection = ref("");
function addSection() {
  const label = newSection.value.trim();
  if (!label) return;
  let key = suggestKey(label) || "section";
  const taken = new Set(sections.value.map((s) => s.key));
  for (let n = 2; taken.has(key); n++) key = `${suggestKey(label) || "section"}_${n}`;
  edit((n) => n.push({ type: "section", key, label, hidden: false, items: [] }));
  newSection.value = "";
}

function resetMenu() {
  props.doc.navigation.entries = [];
}
</script>

<template>
  <LoadingState v-if="classes.isLoading.value" />
  <ErrorAlert v-else-if="classes.isError.value" :error="classes.error.value" :on-retry="() => classes.refetch()" />
  <section v-else class="panel nav-editor">
    <div class="panel-header">
      <h2>Main menu</h2>
      <span class="muted">The menu on the left shows your changes as you make them</span>
    </div>
    <div class="panel-body flush">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">Entry</th>
            <th scope="col">Shown as</th>
            <th scope="col">Visible</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <template v-for="(e, i) in rows" :key="rowId(e)">
            <tr :class="{ 'nav-section': e.type === 'section' }">
              <td>
                <span class="badge">{{ kindLabel(e) }}</span> {{ e.type === "section" ? e.label : defaultName(e) }}
                <span v-if="e.type === 'class' && areaName(e.classKey)" class="muted">· tab {{ areaName(e.classKey) }}</span>
                <span v-if="e.type === 'section'" class="muted">({{ e.items?.length ?? 0 }} classes)</span>
              </td>
              <td>
                <label class="sr-only" :for="`nav-label-${i}`">Name for {{ defaultName(e) }}</label>
                <input
                  :id="`nav-label-${i}`"
                  type="text"
                  maxlength="100"
                  :placeholder="e.type === 'section' ? 'Section heading' : defaultName(e)"
                  :value="e.label ?? ''"
                  @change="setLabel(i, ($event.target as HTMLInputElement).value)"
                />
              </td>
              <td>
                <label class="check">
                  <input type="checkbox" :checked="!e.hidden" @change="setShown(i, ($event.target as HTMLInputElement).checked)" />
                  <span class="sr-only">Show {{ defaultName(e) }}</span>
                </label>
              </td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm btn-icon" :disabled="i === 0" :aria-label="`Move ${defaultName(e)} up`" @click="move(i, i - 1)"><Icon name="arrow-up" /></button>
                <button type="button" class="btn btn-sm btn-icon" :disabled="i === rows.length - 1" :aria-label="`Move ${defaultName(e)} down`" @click="move(i, i + 1)"><Icon name="arrow-down" /></button>
                <select
                  v-if="e.type === 'class' && sections.length > 0"
                  class="inline-select"
                  :aria-label="`Move ${defaultName(e)} into a section`"
                  value=""
                  @change="moveToSection(i, ($event.target as HTMLSelectElement).value)"
                >
                  <option value="">Into section…</option>
                  <option v-for="s in sections" :key="s.key" :value="s.key">{{ s.label }}</option>
                </select>
                <button v-if="e.type === 'section'" type="button" class="btn btn-sm" @click="removeSection(i)">Remove section</button>
              </td>
            </tr>
            <tr v-for="(it, j) in e.items ?? []" :key="`${rowId(e)}/${it.classKey}`" class="nav-item">
              <td><span class="badge">Class</span> {{ className(it.classKey) }}</td>
              <td>
                <label class="sr-only" :for="`nav-label-${i}-${j}`">Name for {{ className(it.classKey) }}</label>
                <input :id="`nav-label-${i}-${j}`" type="text" maxlength="100" :placeholder="className(it.classKey)" :value="it.label ?? ''" @change="setItemLabel(i, j, ($event.target as HTMLInputElement).value)" />
              </td>
              <td>
                <label class="check">
                  <input type="checkbox" :checked="!it.hidden" @change="setItemShown(i, j, ($event.target as HTMLInputElement).checked)" />
                  <span class="sr-only">Show {{ className(it.classKey) }}</span>
                </label>
              </td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm btn-icon" :disabled="j === 0" :aria-label="`Move ${className(it.classKey)} up`" @click="moveItemIn(i, j, j - 1)"><Icon name="arrow-up" /></button>
                <button type="button" class="btn btn-sm btn-icon" :disabled="j === (e.items?.length ?? 0) - 1" :aria-label="`Move ${className(it.classKey)} down`" @click="moveItemIn(i, j, j + 1)"><Icon name="arrow-down" /></button>
                <button type="button" class="btn btn-sm" @click="moveOut(i, j)">Out of section</button>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
    </div>
    <div class="panel-body">
      <form class="inline-control" @submit.prevent="addSection">
        <label for="nav-new-section">New section</label>
        <input id="nav-new-section" v-model="newSection" type="text" maxlength="100" placeholder="e.g. Network" />
        <button type="submit" class="btn" :disabled="!newSection.trim()">Add section</button>
        <span style="flex: 1" />
        <button type="button" class="btn" :disabled="doc.navigation.entries.length === 0" @click="resetMenu">Reset to the built-in menu</button>
      </form>
      <p class="hint">
        Classes appear under their area's tab; the tabs themselves are ordered, renamed and archived in
        <RouterLink to="/admin/areas">Administration › Areas</RouterLink>. A section takes classes out of their tab and
        groups them under its own heading. Classes created later join their area's tab until you place them. Entries a user may not open (Administration, Audit log) stay hidden from them whatever this says.
      </p>
    </div>
  </section>
</template>
