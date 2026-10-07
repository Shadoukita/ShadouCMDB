import type { ProviderKind } from "../../../api/identityProviders";
import { t } from "../../../i18n";
import type { SecretField } from "./secretReentry";

/** "OpenID Connect" or "LDAP / Active Directory", in the active language. */
export const providerKindLabel = (kind: ProviderKind) => (kind === "oidc" ? t("idp.kind.oidc") : t("idp.kind.ldap"));

/** The hint under a secret that must be entered again. */
export const reentryHint = (field: SecretField) => (field === "oidc.clientSecret" ? t("idp.secret.reenterClient") : t("idp.secret.reenterBind"));
