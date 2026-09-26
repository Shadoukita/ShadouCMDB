import { Link } from "react-router-dom";
import { extendTrail, type TrailStep } from "../lib/trail";

/** Link to a CI detail page. Pass `from` + `trail` to carry the walk trail into the breadcrumb. */
export function CiLink({
  id,
  children,
  from,
  trail = [],
}: {
  id: string;
  children: React.ReactNode;
  from?: TrailStep;
  trail?: TrailStep[];
}) {
  return (
    <Link to={`/cis/${id}`} state={from ? { trail: extendTrail(trail, from, id) } : undefined}>
      {children}
    </Link>
  );
}
