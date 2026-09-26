const TONES: Record<string, string> = {
  in_service: "ok",
  active: "ok",
  operational: "ok",
  planned: "neutral",
  ordered: "neutral",
  in_stock: "neutral",
  maintenance: "warn",
  degraded: "warn",
  retired: "off",
  decommissioned: "off",
  disposed: "off",
  failed: "danger",
};

/** Status badge. Tone is a hint derived from the status key; unknown keys stay neutral. */
export function StatusBadge({ status }: { status: { key: string; name: string } | null | undefined }) {
  if (!status) return null;
  return <span className={`badge ${TONES[status.key] ?? ""}`}>{status.name}</span>;
}

export function Badge({ children, tone }: { children: React.ReactNode; tone?: "ok" | "warn" | "off" | "danger" }) {
  return <span className={`badge ${tone ?? ""}`}>{children}</span>;
}
