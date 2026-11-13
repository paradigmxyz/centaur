import { SearchX } from "lucide-react";
import { Link } from "react-router";

import { EmptyState } from "@/components/empty-state";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";

export function NotFound() {
  return (
    <>
      <PageHeader title="Not found" subtitle="This page does not exist or you do not have access to it." />
      <EmptyState className="flex flex-col items-center gap-4 py-12 text-center">
        <SearchX className="size-8 text-muted-foreground/60" aria-hidden="true" />
        <Button asChild variant="outline">
          <Link to="/">Go to the console home</Link>
        </Button>
      </EmptyState>
    </>
  );
}
