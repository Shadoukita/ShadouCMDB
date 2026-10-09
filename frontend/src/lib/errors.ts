import { ApiError } from "../api/client";
import { t } from "../i18n";

/** The API error behind a thrown value, if it is one. */
export function asApiError(error: unknown): ApiError | null {
  return error instanceof ApiError ? error : null;
}

/** The message of any thrown value. */
export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** A short heading that names the kind of failure (ErrorAlert, the boot error card). */
export function errorHeading(e: ApiError | null): string {
  if (!e) return t("error.generic");
  switch (e.code) {
    case "NETWORK_ERROR":
      return t("error.network");
    case "DATABASE_UNAVAILABLE":
      return t("error.databaseUnavailable");
    case "IDENTITY_PROVIDER_UNAVAILABLE":
      return t("error.directoryUnavailable");
    case "SCHEMA_NOT_MIGRATED":
      return t("error.schemaNotMigrated");
    case "NOT_FOUND":
      return t("error.notFound");
    case "VALIDATION_ERROR":
      return t("error.validation");
    case "VERSION_CONFLICT":
      return t("error.versionConflict");
    case "SCHEMA_CHANGE_REFUSED":
      return t("error.schemaChangeRefused");
    case "INVALID_NAME":
      return t("error.invalidName");
    case "CONFLICT":
    case "IN_USE":
      return t("error.conflict");
    case "FORBIDDEN":
    case "UNAUTHORIZED":
      return t("error.forbidden");
    default:
      return t("error.requestFailed", { status: e.status || e.code });
  }
}

/** Retrying a refused request cannot succeed until someone changes the user's profiles. */
export function canRetry(e: ApiError | null): boolean {
  return e?.code !== "FORBIDDEN";
}
