import { VueQueryPlugin } from "@tanstack/vue-query";
import { createPinia } from "pinia";
import { createApp } from "vue";
import { watch } from "vue";
import { onMfaEnrolmentRequired, onSessionEnded } from "./api/client";
import { queryClient } from "./api/queryClient";
import App from "./App.vue";
import { router, TWO_FACTOR_SETUP } from "./router";
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

// A profile the user holds now requires two-factor authentication (made mandatory by an
// administrator, or the user turned theirs off): re-read the session, then set it up.
onMfaEnrolmentRequired(() => {
  const session = useSessionStore();
  if (session.status === "signedIn" && !session.enrolmentRequired) void session.refresh().catch(() => undefined);
});
watch(
  () => useSessionStore().enrolmentRequired,
  (required) => {
    const here = router.currentRoute.value;
    // From sign-in, the router guard sends the user there on the way in.
    if (!required || here.path === TWO_FACTOR_SETUP || here.meta.public) return;
    router.replace({ path: TWO_FACTOR_SETUP, query: here.fullPath === "/" ? {} : { redirect: here.fullPath } });
  },
);

// Branding is public (the sign-in page is branded too); App applies it as it arrives.
void useBrandingStore().load();

app.mount("#app");
