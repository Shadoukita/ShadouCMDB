import { VueQueryPlugin } from "@tanstack/vue-query";
import { createPinia } from "pinia";
import { createApp } from "vue";
import { watch } from "vue";
import { START_LOCATION } from "vue-router";
import { onEmailRequired, onMfaEnrolmentRequired, onReauthenticationRequired, onSessionEnded } from "./api/client";
import { queryClient } from "./api/queryClient";
import App from "./App.vue";
import { listenForLayoutUpdates } from "./lib/layoutEditor";
import { reauthentication } from "./lib/reauthentication";
import { EMAIL_ENTRY, loginQuery, router, TWO_FACTOR_SETUP } from "./router";
import { useBrandingStore } from "./stores/branding";
import { useSessionStore } from "./stores/session";
import "./styles/app.css";

const app = createApp(App).use(createPinia()).use(router).use(VueQueryPlugin, { queryClient });

// A layout saved in the layout editor's window shows at once in this one.
listenForLayoutUpdates(queryClient);

// Any 401 on a signed-in request means the session ended (idle/absolute timeout,
// signed out elsewhere, account disabled): go to sign-in, then come back here.
onSessionEnded(() => {
  const session = useSessionStore();
  if (session.status !== "signedIn") return;
  session.markExpired();
  const here = router.currentRoute.value;
  router.replace({ path: "/login", query: here.meta.public ? {} : loginQuery(here) });
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
    // From sign-in, and on a page load (the session is read during the first navigation), the router
    // guard sends the user there on the way in, keeping ?redirect; replacing it here would drop that.
    if (!required || here === START_LOCATION || here.path === TWO_FACTOR_SETUP || here.meta.public) return;
    router.replace({ path: TWO_FACTOR_SETUP, query: here.fullPath === "/" ? {} : { redirect: here.fullPath } });
  },
);

// The account has no e-mail yet (created before e-mails were required, and the session was read
// before that showed): re-read the session, then ask for it. The router guard keeps the user there.
onEmailRequired(() => {
  const session = useSessionStore();
  if (session.status === "signedIn" && !session.emailRequired) void session.refresh().catch(() => undefined);
});
watch(
  () => useSessionStore().emailRequired,
  (required) => {
    const here = router.currentRoute.value;
    if (!required || here === START_LOCATION || here.path === EMAIL_ENTRY || here.meta.public) return;
    router.replace({ path: EMAIL_ENTRY, query: here.fullPath === "/" ? {} : { redirect: here.fullPath } });
  },
);

// A change to accounts, profiles, API tokens or identity providers needs the password confirmed
// in the last 10 minutes (GH#498): ask for it, then the user sends the change again.
onReauthenticationRequired(() => {
  if (useSessionStore().status === "signedIn") reauthentication.open = true;
});

// Branding is public (the sign-in page is branded too); App applies it as it arrives.
void useBrandingStore().load();

app.mount("#app");
