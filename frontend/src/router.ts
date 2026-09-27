import { createRouter, createWebHistory } from "vue-router";
import CiCreatePage from "./pages/CiCreatePage.vue";
import CiDetailPage from "./pages/CiDetailPage.vue";
import CiEditPage from "./pages/CiEditPage.vue";
import DashboardPage from "./pages/DashboardPage.vue";
import InventoryPage from "./pages/InventoryPage.vue";
import NotFoundPage from "./pages/NotFoundPage.vue";
import SearchPage from "./pages/SearchPage.vue";
import AdminLayout from "./pages/admin/AdminLayout.vue";
import ApiTokensPage from "./pages/admin/ApiTokensPage.vue";
import IdentityProviderEditPage from "./pages/admin/identity/IdentityProviderEditPage.vue";
import IdentityProvidersPage from "./pages/admin/identity/IdentityProvidersPage.vue";
import AuditLogPage from "./pages/admin/AuditLogPage.vue";
import TemplatesPage from "./pages/admin/TemplatesPage.vue";
import ConfigTransferPage from "./pages/admin/config/ConfigTransferPage.vue";
import CustomizationPage from "./pages/admin/customization/CustomizationPage.vue";
import AreasPage from "./pages/admin/datamodel/AreasPage.vue";
import ClassEditPage from "./pages/admin/datamodel/ClassEditPage.vue";
import ClassesPage from "./pages/admin/datamodel/ClassesPage.vue";
import RelationshipTypesPage from "./pages/admin/datamodel/RelationshipTypesPage.vue";
import LookupsPage from "./pages/admin/lookups/LookupsPage.vue";
import ProfileEditPage from "./pages/admin/ProfileEditPage.vue";
import ProfilesPage from "./pages/admin/ProfilesPage.vue";
import UserEditPage from "./pages/admin/UserEditPage.vue";
import UsersPage from "./pages/admin/UsersPage.vue";
import { ADMIN_SECTIONS, visibleSections } from "./pages/admin/sections";
import AccountPage from "./pages/account/AccountPage.vue";
import TwoFactorSetupPage from "./pages/account/TwoFactorSetupPage.vue";
import LoginPage from "./pages/auth/LoginPage.vue";
import SetupPage from "./pages/auth/SetupPage.vue";
import { trackNavigations } from "./lib/navigation";
import { useSessionStore } from "./stores/session";
import type { GlobalPermission } from "./api/admin";

declare module "vue-router" {
  interface RouteMeta {
    /** Reachable without a session (sign-in, first-run setup); rendered without the app shell. */
    public?: boolean;
    /** Needs a session but is rendered without the app shell (forced two-factor set-up). */
    bare?: boolean;
    /** Administration screens: the user needs any one of these. */
    permissions?: GlobalPermission[];
    /** Administration screens only for holders of the built-in Administrator profile. */
    administratorOnly?: boolean;
  }
}

/** Where a user goes while a profile they hold requires two-factor authentication they have not set up. */
export const TWO_FACTOR_SETUP = "/two-factor-setup";

const section = (key: string) => ADMIN_SECTIONS.find((s) => s.key === key)!.permissions;

export const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    { path: "/login", component: LoginPage, meta: { public: true } },
    { path: "/setup", component: SetupPage, meta: { public: true } },
    { path: "/", component: DashboardPage },
    { path: "/cis", component: InventoryPage },
    { path: "/cis/new", component: CiCreatePage },
    { path: "/cis/:id", component: CiDetailPage },
    { path: "/cis/:id/edit", component: CiEditPage },
    { path: "/search", component: SearchPage },
    { path: "/account", component: AccountPage },
    { path: TWO_FACTOR_SETUP, component: TwoFactorSetupPage, meta: { bare: true } },
    {
      path: "/admin",
      component: AdminLayout,
      children: [
        {
          path: "",
          // Opens the first section the user may use; with none, AdminLayout explains the missing permission.
          component: { render: () => null },
          beforeEnter: () => visibleSections(useSessionStore().adminAccess)[0]?.to ?? true,
        },
        { path: "users", component: UsersPage, meta: { permissions: section("users") } },
        { path: "users/new", component: UserEditPage, meta: { permissions: section("users") } },
        { path: "users/:id", component: UserEditPage, meta: { permissions: section("users") } },
        { path: "profiles", component: ProfilesPage, meta: { permissions: section("profiles") } },
        { path: "profiles/new", component: ProfileEditPage, meta: { permissions: ["profiles.manage"] } },
        { path: "profiles/:id", component: ProfileEditPage, meta: { permissions: section("profiles") } },
        { path: "api-tokens", component: ApiTokensPage, meta: { permissions: section("api-tokens") } },
        { path: "identity-providers", component: IdentityProvidersPage, meta: { administratorOnly: true } },
        { path: "identity-providers/new", component: IdentityProviderEditPage, meta: { administratorOnly: true } },
        { path: "identity-providers/:id", component: IdentityProviderEditPage, meta: { administratorOnly: true } },
        { path: "areas", component: AreasPage, meta: { permissions: section("areas") } },
        { path: "classes", component: ClassesPage, meta: { permissions: section("classes") } },
        { path: "classes/new", component: ClassEditPage, meta: { permissions: section("classes") } },
        { path: "classes/:id", component: ClassEditPage, meta: { permissions: section("classes") } },
        { path: "relationships", component: RelationshipTypesPage, meta: { permissions: section("relationships") } },
        { path: "lookups", redirect: "/admin/lookups/statuses" },
        { path: "lookups/:kind", component: LookupsPage, meta: { permissions: section("lookups") } },
        { path: "templates", component: TemplatesPage, meta: { permissions: section("templates") } },
        { path: "customization", redirect: "/admin/customization/branding" },
        { path: "customization/:section", component: CustomizationPage, meta: { permissions: section("customization") } },
        { path: "config", component: ConfigTransferPage, meta: { permissions: section("config") } },
        { path: "audit", component: AuditLogPage, meta: { permissions: section("audit") } },
      ],
    },
    { path: "/:pathMatch(.*)*", component: NotFoundPage },
  ],
  scrollBehavior: (_to, _from, saved) => saved ?? { top: 0 },
});

trackNavigations(router);

/** Only same-app paths are followed after sign-in (never another origin). */
export function safeRedirect(value: unknown): string {
  return typeof value === "string" && value.startsWith("/") && !value.startsWith("//") ? value : "/";
}

// Setup → sign-in → app. The API enforces every permission; this only decides which screen to show.
router.beforeEach(async (to) => {
  const session = useSessionStore();
  await session.ensureLoaded();
  if (session.status === "unknown") return true; // API unreachable: App shows the error with Retry
  if (session.status === "setup") return to.path === "/setup" ? true : "/setup";
  if (session.status === "anonymous") {
    if (to.path === "/login") return true;
    if (to.path === "/setup" || to.meta.public) return "/login";
    return { path: "/login", query: to.fullPath === "/" ? {} : { redirect: to.fullPath } };
  }
  if (to.meta.public) return safeRedirect(to.query.redirect);
  // Until the required two-factor set-up is done, the API refuses everything else (403 MFA_ENROLMENT_REQUIRED).
  if (session.enrolmentRequired && to.path !== TWO_FACTOR_SETUP) {
    return { path: TWO_FACTOR_SETUP, query: to.fullPath === "/" ? {} : { redirect: to.fullPath } };
  }
  if (!session.enrolmentRequired && to.path === TWO_FACTOR_SETUP) return "/account";
  return true;
});
