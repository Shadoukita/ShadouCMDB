<script setup lang="ts">
import { useQueryClient } from "@tanstack/vue-query";
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { api, ApiError, unwrap } from "../api/client";
import { importKeys } from "../api/imports";
import {
  NOTIFICATIONS_MAX,
  NOTIFICATIONS_PAGE,
  useDismissNotification,
  useMarkAllNotificationsRead,
  useNotifications,
  useSetNotificationRead,
  useUnreadNotificationCount,
  type Notification,
  type NotificationKind,
} from "../api/notifications";
import { runtimeKeys } from "../api/workflowRuntime";
import { t } from "../i18n";
import { formatDateTime, formatRelative } from "../lib/format";
import { badgeText, describeNotification, notificationTarget } from "../lib/notifications";
import type { IconName } from "../icons/lucide";
import ErrorAlert from "./ErrorAlert.vue";
import Icon from "./Icon.vue";
import LoadingState from "./LoadingState.vue";

/**
 * The header bell (design §0.6 gap G5, SHAA-2356): the unread count, polled while the tab is visible, and a panel
 * with the newest notifications. Opening one marks it read and goes to what it is about; that record may be gone
 * by now (no foreign key by design) or hidden from the caller, which the panel says instead of opening an error page.
 */
const route = useRoute();
const router = useRouter();
const qc = useQueryClient();

const open = ref(false);
const root = ref<HTMLElement>();
const toggle = ref<HTMLButtonElement>();
const limit = ref(NOTIFICATIONS_PAGE);

const count = useUnreadNotificationCount();
const unread = computed(() => count.data.value?.unread ?? 0);
const list = useNotifications(limit, open);
const rows = computed(() => list.data.value?.data ?? []);
const items = computed(() => rows.value.map((n) => ({ n, target: notificationTarget(n), ...describeNotification(n) })));
const total = computed(() => list.data.value?.page.total ?? 0);
const canShowMore = computed(() => rows.value.length < total.value && limit.value < NOTIFICATIONS_MAX);

const markAll = useMarkAllNotificationsRead();
const setRead = useSetNotificationRead();
const dismiss = useDismissNotification();
const actionError = ref<unknown>(null);
/** Notifications whose record turned out to be gone when the user tried to open it. */
const unavailable = reactive(new Set<string>());

const KIND_ICONS: Record<NotificationKind, IconName> = {
  approval_requested: "shield",
  approval_closed: "circle-check",
  workflow_transition: "arrow-right",
  import_finished: "upload",
  workflow_action: "bell",
  webhook_suspended: "triangle-alert",
};

// A new arrival changes the count: refresh an open list with it.
watch(unread, () => {
  if (open.value) void qc.invalidateQueries({ queryKey: ["notifications", "list"] });
});
watch(() => route.fullPath, () => (open.value = false));
watch(open, (o) => {
  if (!o) {
    limit.value = NOTIFICATIONS_PAGE;
    actionError.value = null;
  }
});

function onDocClick(e: MouseEvent) {
  if (open.value && !root.value?.contains(e.target as Node)) open.value = false;
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape" && open.value) {
    e.stopPropagation();
    open.value = false;
    toggle.value?.focus();
  }
}
onMounted(() => document.addEventListener("click", onDocClick));
onBeforeUnmount(() => document.removeEventListener("click", onDocClick));

async function run(p: Promise<unknown>) {
  actionError.value = null;
  try {
    await p;
  } catch (e) {
    actionError.value = e;
  }
}

function markAllRead() {
  // Only up to the newest one shown: something that arrived since stays unread.
  void run(markAll.mutateAsync(rows.value[0]?.createdAt));
}
function toggleRead(n: Notification) {
  void run(setRead.mutateAsync({ id: n.id, read: !n.readAt }));
}
function dismissOne(n: Notification) {
  void run(dismiss.mutateAsync(n.id));
}

const isGone = (e: unknown) =>
  e instanceof ApiError && (e.status === 404 || (e.code === "VALIDATION_ERROR" && e.details.some((x) => x.in === "params")));

/** Reads the target through the query its page uses (so the page opens from cache) and says whether it still exists. */
async function targetExists(target: string): Promise<boolean> {
  const [, area, rawId] = target.split("/");
  const id = decodeURIComponent(rawId ?? "");
  try {
    if (area === "imports")
      await qc.fetchQuery({ queryKey: importKeys.job(id), queryFn: () => unwrap(api.GET("/api/v1/imports/{id}", { params: { path: { id } } })) });
    else
      await qc.fetchQuery({
        queryKey: runtimeKeys.instance(id),
        queryFn: () => unwrap(api.GET("/api/v1/workflow-instances/{id}", { params: { path: { id } } })),
      });
    return true;
  } catch (e) {
    // Anything but "gone" (a network failure, a 5xx) is the target page's to show.
    return !isGone(e);
  }
}

async function openNotification(n: Notification, target: string, e: MouseEvent) {
  if (!n.readAt) void run(setRead.mutateAsync({ id: n.id, read: true }));
  // Ctrl/Cmd/Shift-click and middle-click open a new tab or window as links do.
  if (e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
  e.preventDefault();
  if (await targetExists(target)) {
    open.value = false;
    await router.push(target);
  } else {
    unavailable.add(n.id);
  }
}
</script>

<template>
  <div ref="root" class="notif-menu" @keydown="onKeydown">
    <button
      ref="toggle"
      type="button"
      class="notif-bell"
      aria-haspopup="true"
      aria-controls="notif-panel"
      :aria-expanded="open"
      :aria-label="unread ? t('notifications.bellUnread', { n: unread }) : t('notifications.bell')"
      :title="unread ? t('notifications.bellUnread', { n: unread }) : t('notifications.bell')"
      @click="open = !open"
    >
      <Icon name="bell" :size="18" />
      <span v-if="unread" class="notif-badge" aria-hidden="true">{{ badgeText(unread) }}</span>
    </button>
    <section v-show="open" id="notif-panel" class="notif-panel" :aria-label="t('notifications.title')">
      <header class="notif-head">
        <h2>{{ t("notifications.title") }}</h2>
        <button type="button" class="btn btn-ghost btn-sm" :disabled="!unread || markAll.isPending.value" @click="markAllRead">
          <Icon name="check-check" />{{ t("notifications.markAllRead") }}
        </button>
      </header>
      <ErrorAlert v-if="actionError" :error="actionError" :title="t('notifications.actionFailed')" />
      <LoadingState v-if="list.isPending.value" />
      <ErrorAlert v-else-if="list.isError.value" :error="list.error.value" :title="t('notifications.loadFailed')" :on-retry="() => list.refetch()" />
      <div v-else-if="!rows.length" class="notif-empty">
        <Icon name="bell" :size="20" />
        <strong>{{ t("notifications.empty.title") }}</strong>
        <p>{{ t("notifications.empty.body") }}</p>
      </div>
      <ul v-else class="notif-list">
        <li v-for="{ n, target, title, detail } in items" :key="n.id" class="notif-item" :class="{ 'is-unread': !n.readAt }">
          <span class="notif-icon"><Icon :name="KIND_ICONS[n.kind] ?? 'info'" /></span>
          <div class="notif-body">
            <span class="notif-kind">
              <span v-if="!n.readAt" class="notif-dot" :title="t('notifications.unread')"><span class="sr-only">{{ t("notifications.unread") }}: </span></span>
              {{ t(`notifications.kind.${n.kind}`) }}
            </span>
            <RouterLink v-if="target && !unavailable.has(n.id)" class="notif-title" :to="target" @click="openNotification(n, target, $event)">
              {{ title }}
            </RouterLink>
            <span v-else class="notif-title">{{ title }}</span>
            <span v-if="detail" class="notif-detail">{{ detail }}</span>
            <span class="notif-meta">
              <time :datetime="n.createdAt" :title="formatDateTime(n.createdAt)">{{ formatRelative(n.createdAt) }}</time>
              <span v-if="!target || unavailable.has(n.id)" class="notif-gone" :title="t('notifications.unavailableTitle')">
                {{ t("notifications.unavailable") }}
              </span>
            </span>
          </div>
          <div class="notif-actions">
            <button
              type="button"
              class="btn btn-ghost btn-sm btn-icon"
              :aria-label="n.readAt ? t('notifications.markUnread') : t('notifications.markRead')"
              :title="n.readAt ? t('notifications.markUnread') : t('notifications.markRead')"
              @click="toggleRead(n)"
            >
              <Icon :name="n.readAt ? 'circle' : 'check'" />
            </button>
            <button type="button" class="btn btn-ghost btn-sm btn-icon" :aria-label="t('notifications.dismiss')" :title="t('notifications.dismiss')" @click="dismissOne(n)">
              <Icon name="x" />
            </button>
          </div>
        </li>
      </ul>
      <button v-if="canShowMore" type="button" class="btn btn-ghost btn-sm notif-more" @click="limit = Math.min(limit + NOTIFICATIONS_PAGE, NOTIFICATIONS_MAX)">
        {{ t("notifications.showMore") }}
      </button>
    </section>
  </div>
</template>
