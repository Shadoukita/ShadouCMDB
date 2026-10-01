// TanStack Query composables for user groups (Administration › Access › Groups, `users.manage`).
// Same rules as admin.ts: every request goes through the typed client, and mutations
// invalidate exactly what they change.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody as Body, type Schemas } from "./client";
import { MAX_PAGE } from "./queries";
import type { paths } from "./schema";

export type UserGroup = Schemas["UserGroup"];
export type UserGroupDetail = Schemas["UserGroupDetail"];
export type UserGroupMember = Schemas["UserGroupMember"];
export type GroupCreateBody = Body<"/api/v1/admin/groups", "post">;
export type GroupUpdateBody = Body<"/api/v1/admin/groups/{id}", "patch">;
export type GroupListQuery = NonNullable<paths["/api/v1/admin/groups"]["get"]["parameters"]["query"]>;
export type GroupMemberQuery = NonNullable<paths["/api/v1/admin/groups/{id}/members"]["get"]["parameters"]["query"]>;

/** The most members a group can have: one replace call carries the whole set. */
export const MAX_GROUP_MEMBERS = 1000;

export const groupKeys = {
  all: ["admin", "groups"] as const,
  list: (q: GroupListQuery) => ["admin", "groups", "list", q] as const,
  detail: (id: string) => ["admin", "groups", "detail", id] as const,
  members: (id: string) => ["admin", "groups", "members", id] as const,
  memberPage: (id: string, q: GroupMemberQuery) => ["admin", "groups", "members", id, q] as const,
};

export function useGroupList(query: MaybeRefOrGetter<GroupListQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: groupKeys.list(q),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/groups", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useGroup(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const groupId = toValue(id) ?? "";
    return {
      queryKey: groupKeys.detail(groupId),
      enabled: !!groupId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/groups/{id}", { params: { path: { id: groupId } }, signal })),
    };
  });
}

export function useGroupMembers(id: MaybeRefOrGetter<string | undefined>, query: MaybeRefOrGetter<GroupMemberQuery>) {
  return useQuery(() => {
    const groupId = toValue(id) ?? "";
    const q = toValue(query);
    return {
      queryKey: groupKeys.memberPage(groupId, q),
      enabled: !!groupId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/groups/{id}/members", { params: { path: { id: groupId }, query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** Every member's id, page by page (at most MAX_GROUP_MEMBERS), as the base of a replace. */
async function allMemberIds(id: string): Promise<string[]> {
  const ids: string[] = [];
  for (let offset = 0; ; offset += MAX_PAGE) {
    const page = await unwrap(
      api.GET("/api/v1/admin/groups/{id}/members", { params: { path: { id }, query: { limit: MAX_PAGE, offset, sort: "username" } } }),
    );
    ids.push(...page.data.map((m) => m.id));
    if (page.data.length < MAX_PAGE || ids.length >= page.page.total) return ids;
  }
}

/** Group rows and the group's detail carry the member count and version: refresh both. */
function useGroupMutation<V>(fn: (vars: V) => Promise<UserGroup>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: (group) => {
      qc.invalidateQueries({ queryKey: groupKeys.all });
      qc.setQueryData<UserGroupDetail>(groupKeys.detail(group.id), (old) => (old ? { ...old, ...group } : undefined));
    },
  });
}

export const useCreateGroup = () => useGroupMutation((body: GroupCreateBody) => unwrap(api.POST("/api/v1/admin/groups", { body })));

export const useUpdateGroup = () =>
  useGroupMutation(({ id, body }: { id: string; body: GroupUpdateBody }) =>
    unwrap(api.PATCH("/api/v1/admin/groups/{id}", { params: { path: { id } }, body })),
  );

/**
 * Adds and removes members. The API replaces the whole set, so this reads the current members first and
 * sends them back with the change; `version` makes a concurrent edit fail with 409 instead of being lost.
 */
export const useChangeGroupMembers = () =>
  useGroupMutation(async ({ id, version, add = [], remove = [] }: { id: string; version: number; add?: string[]; remove?: string[] }) => {
    const drop = new Set(remove);
    const next = [...new Set([...(await allMemberIds(id)), ...add])].filter((u) => !drop.has(u));
    return unwrap(api.PUT("/api/v1/admin/groups/{id}/members", { params: { path: { id } }, body: { version, userIds: next } }));
  });

export const useDeleteGroup = () => {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/admin/groups/{id}", { params: { path: { id } } })),
    onSuccess: (_result, id) => {
      qc.removeQueries({ queryKey: groupKeys.detail(id) });
      qc.invalidateQueries({ queryKey: groupKeys.all });
    },
  });
};
