import { Link } from "react-router";

import { EmptyState } from "@/components/empty-state";
import { PageHeader } from "@/components/page-header";

export function NotFound() {
  return (
    <>
      <PageHeader title="Not found" subtitle="This page does not exist or you do not have access to it." />
      <EmptyState>
        <Link to="/" className="text-primary hover:underline">
          Go to the console home
        </Link>
      </EmptyState>
    </>
  );
}
