import { createRouter, createWebHistory } from "vue-router";
import CiCreatePage from "./pages/CiCreatePage.vue";
import CiDetailPage from "./pages/CiDetailPage.vue";
import CiEditPage from "./pages/CiEditPage.vue";
import DashboardPage from "./pages/DashboardPage.vue";
import InventoryPage from "./pages/InventoryPage.vue";
import NotFoundPage from "./pages/NotFoundPage.vue";
import SearchPage from "./pages/SearchPage.vue";

export const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    { path: "/", component: DashboardPage },
    { path: "/cis", component: InventoryPage },
    { path: "/cis/new", component: CiCreatePage },
    { path: "/cis/:id", component: CiDetailPage },
    { path: "/cis/:id/edit", component: CiEditPage },
    { path: "/search", component: SearchPage },
    { path: "/:pathMatch(.*)*", component: NotFoundPage },
  ],
  scrollBehavior: (_to, _from, saved) => saved ?? { top: 0 },
});
