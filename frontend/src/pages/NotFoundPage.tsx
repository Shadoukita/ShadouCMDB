import { Link } from "react-router-dom";
import { Breadcrumbs } from "../components/Breadcrumbs";
import { EmptyState } from "../components/States";

export function NotFoundPage() {
  return (
    <>
      <Breadcrumbs items={[{ label: "Not found" }]} />
      <EmptyState title="Page not found" actions={<Link className="btn" to="/">Go to the dashboard</Link>}>
        This address does not match any screen.
      </EmptyState>
    </>
  );
}
