// TanStack Query composables for the signed-in user's in-app notifications (/notifications, SHAA-2356). Session
// only and scoped to the caller: another user's notification is a 404 like one that does not exist. Nothing is
// pushed: the bell polls the unread count, and the list is read when the panel opens.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, ApiError, unwrap, type Schemas } from "./client";

export type Notification = Schemas["Notification"];
export type NotificationKind = Schemas["NotificationKind"];

/** How often the bell asks for the unread count while the tab is visible. */
export const UNREAD_POLL_MS = 60_000;
/** The panel's first page; "Show more" adds pages of this size up to the API's limit of 100. */
export const NOTIFICATIONS_PAGE = 20;
export const NOTIFICATIONS_MAX = 100;

export const notificationKeys = {
  all: ["notifications"] as const,
  unread: ["notifications", "unread"] as const,
  list: (limit: number) => ["notifications", "list", limit] as const,
};

export function useUnreadNotificationCount() {
  return useQuery({
    queryKey: notificationKeys.unread,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/notifications/unread-count", { signal })),
    staleTime: 0,
    // The next poll is the retry; a 4xx (e.g. an API token session) will not fix itself.
    retry: false,
    refetchInterval: (query) => {
      const e = query.state.error;
      return e instanceof ApiError && e.status > 0 && e.status < 500 ? false : UNREAD_POLL_MS;
    },
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });
}

/** The newest `limit` notifications; only read while the panel is open. */
export function useNotifications(limit: MaybeRefOrGetter<number>, enabled: MaybeRefOrGetter<boolean>) {
  return useQuery(() => {
    const l = toValue(limit);
    return {
      queryKey: notificationKeys.list(l),
      enabled: toValue(enabled),
      staleTime: 0,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/notifications", { params: { query: { limit: l, offset: 0 } }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

function useNotificationMutation<V>(fn: (vars: V) => Promise<unknown>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    // A 404 means the list is stale too (dismissed in another tab, or the CI's class no longer visible).
    onSettled: () => qc.invalidateQueries({ queryKey: notificationKeys.all }),
  });
}

/** Everything unread, or only what was created up to `upTo` (the newest one the user saw), as read. */
export function useMarkAllNotificationsRead() {
  return useNotificationMutation((upTo: string | undefined) =>
    unwrap(api.POST("/api/v1/notifications/mark-read", { body: upTo ? { upTo } : {} })),
  );
}

export function useSetNotificationRead() {
  return useNotificationMutation((v: { id: string; read: boolean }) =>
    unwrap(api.PATCH("/api/v1/notifications/{id}", { params: { path: { id: v.id } }, body: { read: v.read } })),
  );
}

export function useDismissNotification() {
  return useNotificationMutation((id: string) => unwrap(api.DELETE("/api/v1/notifications/{id}", { params: { path: { id } } })));
}
