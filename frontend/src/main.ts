import { VueQueryPlugin } from "@tanstack/vue-query";
import { createPinia } from "pinia";
import { createApp } from "vue";
import { onSessionEnded } from "./api/client";
import { queryClient } from "./api/queryClient";
import App from "./App.vue";
import { router } from "./router";
import { useBrandingStore } from "./stores/branding";
import { useSessionStore } from "./stores/session";
import "./styles/app.css";

const app = createApp(App).use(createPinia()).use(router).use(VueQueryPlugin, { queryClient });

// Any 401 on a signed-in request means the session ended (idle/absolute timeout,
// signed out elsewhere, account disabled): go to sign-in, then come back here.
onSessionEnded(() => {
  const session = useSessionStore();
  if (session.status !== "signedIn") return;
  session.markExpired();
  const here = router.currentRoute.value;
  router.replace({ path: "/login", query: here.meta.public ? {} : { redirect: here.fullPath } });
});

// Branding is public (the sign-in page is branded too); App applies it as it arrives.
void useBrandingStore().load();

app.mount("#app");
