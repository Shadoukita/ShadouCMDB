import { createRouter, createWebHistory, type RouteLocationNormalized } from "vue-router";
import CiCreatePage from "./pages/CiCreatePage.vue";
import CiDetailPage from "./pages/CiDetailPage.vue";
import CiEditPage from "./pages/CiEditPage.vue";
import DashboardPage from "./pages/DashboardPage.vue";
import InventoryPage from "./pages/InventoryPage.vue";
import NotFoundPage from "./pages/NotFoundPage.vue";
import SearchPage from "./pages/SearchPage.vue";
import ServiceDetailPage from "./pages/services/ServiceDetailPage.vue";
import ServiceListPage from "./pages/services/ServiceListPage.vue";
import AdminLayout from "./pages/admin/AdminLayout.vue";
import ApiTokensPage from "./pages/admin/ApiTokensPage.vue";
import IdentityProviderEditPage from "./pages/admin/identity/IdentityProviderEditPage.vue";
import IdentityProvidersPage from "./pages/admin/identity/IdentityProvidersPage.vue";
import AuditLogPage from "./pages/admin/AuditLogPage.vue";
import TemplatesPage from "./pages/admin/TemplatesPage.vue";
import ImportSettingsPage from "./pages/admin/ImportSettingsPage.vue";
import ImportsPage from "./pages/imports/ImportsPage.vue";
import ImportWizardPage from "./pages/imports/ImportWizardPage.vue";
import ConfigTransferPage from "./pages/admin/config/ConfigTransferPage.vue";
import CustomizationPage from "./pages/admin/customization/CustomizationPage.vue";
import AreasPage from "./pages/admin/datamodel/AreasPage.vue";
import ClassEditPage from "./pages/admin/datamodel/ClassEditPage.vue";
import ClassesPage from "./pages/admin/datamodel/ClassesPage.vue";
import DropdownsPage from "./pages/admin/datamodel/DropdownsPage.vue";
import RelationshipTypesPage from "./pages/admin/datamodel/RelationshipTypesPage.vue";
import ProfileEditPage from "./pages/admin/ProfileEditPage.vue";
import ProfilesPage from "./pages/admin/ProfilesPage.vue";
import UserEditPage from "./pages/admin/UserEditPage.vue";
import UsersPage from "./pages/admin/UsersPage.vue";
import GroupEditPage from "./pages/admin/GroupEditPage.vue";
import GroupsPage from "./pages/admin/GroupsPage.vue";
import { ADMIN_SECTIONS, visibleSections } from "./pages/admin/sections";
import AccountPage from "./pages/account/AccountPage.vue";
import TwoFactorSetupPage from "./pages/account/TwoFactorSetupPage.vue";
import LoginPage from "./pages/auth/LoginPage.vue";
import SetupPage from "./pages/auth/SetupPage.vue";
import { EDITOR_SUFFIX, OPENED_HERE_QUERY, pageOfEditor } from "./lib/layoutEditor";
import { trackNavigations } from "./lib/navigation";
import { safeRedirect } from "./lib/signIn";
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
    /** The layout editor of a CI page (lib/layoutEditor): needs customization.manage. */
    layoutEditor?: boolean;
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
    // The CI's Impact tab: its own URL, so an analysis (with its options in the query) can be bookmarked.
    { path: "/cis/:id/impact", component: CiDetailPage },
    // The layout editor, opened in its own window from the pages above (lib/layoutEditor).
    { path: `/cis/new${EDITOR_SUFFIX}`, component: CiCreatePage, meta: { layoutEditor: true } },
    { path: `/cis/:id${EDITOR_SUFFIX}`, component: CiDetailPage, meta: { layoutEditor: true } },
    { path: `/cis/:id/edit${EDITOR_SUFFIX}`, component: CiEditPage, meta: { layoutEditor: true } },
    // Business services (CIs of the built-in service class): /cis/:id of a service redirects here (CiDetailPage).
    { path: "/services", component: ServiceListPage },
    { path: "/services/:id", component: ServiceDetailPage },
    // The service's Impact tab, defaulting to Upstream; its own URL like a CI's.
    { path: "/services/:id/impact", component: ServiceDetailPage },
    { path: "/search", component: SearchPage },
    { path: "/imports", component: ImportsPage },
    { path: "/imports/new", component: ImportWizardPage },
    { path: "/imports/:id", component: ImportWizardPage },
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
        { path: "groups", component: GroupsPage, meta: { permissions: section("groups") } },
        { path: "groups/new", component: GroupEditPage, meta: { permissions: section("groups") } },
        { path: "groups/:id", component: GroupEditPage, meta: { permissions: section("groups") } },
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
        { path: "dropdowns", component: DropdownsPage, meta: { permissions: section("dropdowns") } },
        // Lookup lists moved to Dropdowns, and the read-only tabs of the former status, environment, location and
        // owner tables are gone (their values are the lookup lists of the same name): bookmarks land on Dropdowns.
        // /admin/lookups/lists?list=… keeps its list.
        { path: "lookups/:kind?", redirect: (to) => ({ path: "/admin/dropdowns", query: to.params.kind === "lists" ? to.query : {} }) },
        { path: "templates", component: TemplatesPage, meta: { permissions: section("templates") } },
        { path: "customization", redirect: "/admin/customization/branding" },
        { path: "customization/:section", component: CustomizationPage, meta: { permissions: section("customization") } },
        { path: "import", component: ImportSettingsPage, meta: { administratorOnly: true } },
        { path: "config", component: ConfigTransferPage, meta: { permissions: section("config") } },
        { path: "audit", component: AuditLogPage, meta: { permissions: section("audit") } },
      ],
    },
    { path: "/:pathMatch(.*)*", component: NotFoundPage },
  ],
  scrollBehavior: (_to, _from, saved) => saved ?? { top: 0 },
});

trackNavigations(router);


/**
 * The sign-in query that brings the user back to `route`. The two-factor set-up is only a stop on the
 * way (after sign-in the guard sends a user without the requirement to /account), so its own
 * destination is kept instead.
 */
export function loginQuery(route: RouteLocationNormalized): { redirect?: string } {
  const back = route.path === TWO_FACTOR_SETUP ? safeRedirect(route.query.redirect) : route.fullPath;
  return back === "/" ? {} : { redirect: back };
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
    return { path: "/login", query: loginQuery(to) };
  }
  if (to.meta.public) return safeRedirect(to.query.redirect);
  // Until the required two-factor set-up is done, the API refuses everything else (403 MFA_ENROLMENT_REQUIRED).
  if (session.enrolmentRequired && to.path !== TWO_FACTOR_SETUP) {
    return { path: TWO_FACTOR_SETUP, query: to.fullPath === "/" ? {} : { redirect: to.fullPath } };
  }
  if (!session.enrolmentRequired && to.path === TWO_FACTOR_SETUP) return "/account";
  // Without customization.manage the layout editor is just the page.
  if (to.meta.layoutEditor && !session.can("customization.manage")) {
    const query = { ...to.query };
    delete query[OPENED_HERE_QUERY];
    return { path: pageOfEditor(to.path), query, replace: true };
  }
  return true;
});
