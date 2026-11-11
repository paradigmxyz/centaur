import { AppLink } from "@/components/app-link";

/** "← back to principals" */
export function BackLink({ to, children }: { to: string; children: string }) {
  return (
    <AppLink to={to} className="text-xs text-muted-foreground hover:text-centaur-400">
      ← {children}
    </AppLink>
  );
}
