import { useMemo } from "react";
import { toast } from "sonner";

import { StatusBadge, userStatusTone } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { pluralize } from "@/lib/format";

import { useUserAction, useUsers, type ConsoleUser, type UserAction } from "../api";

const SUCCESS: Record<UserAction, (user: ConsoleUser) => string> = {
  approve: (user) => `Approved ${user.email}.`,
  disable: (user) => `Disabled ${user.email}.`,
  promote: (user) => `${user.email} is now an admin.`,
  demote: (user) => `${user.email} is now a user.`,
};

const actionButton = "hover:border-primary/40 hover:text-centaur-300";

export default function UsersPageRoute() {
  return (
    <RequireAccess allowed={(me) => me.nav.users}>
      <UsersPage />
    </RequireAccess>
  );
}

/** One pill per linked identity provider, or "Password" for password-only accounts. */
function IdpChips({ user }: { user: ConsoleUser }) {
  const labels = user.identity_providers.length > 0 ? user.identity_providers : ["password"];
  return (
    <div className="flex flex-wrap gap-1">
      {labels.map((label) => (
        <span key={label} className="rounded-sm bg-muted px-1.5 py-0.5 text-xs text-zinc-300 ring-1 ring-border light:text-zinc-700">
          {label.charAt(0).toUpperCase() + label.slice(1)}
        </span>
      ))}
    </div>
  );
}

function EmailCell({ user }: { user: ConsoleUser }) {
  return (
    <>
      <div className="text-heading">
        {user.email}
        {user.current && <span className="ml-2 text-xs text-subtle-foreground">(you)</span>}
      </div>
      {user.name && <div className="text-xs text-subtle-foreground">{user.name}</div>}
    </>
  );
}

function UsersPage() {
  const users = useUsers();
  const action = useUserAction();

  const run = async (user: ConsoleUser, name: UserAction) => {
    try {
      await action.mutateAsync({ user, action: name });
      toast.success(SUCCESS[name](user));
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const { pending, approved } = useMemo(() => {
    const all = users.data ?? [];
    return {
      pending: all
        .filter((user) => user.status === "pending")
        .sort((a, b) => a.created_at.localeCompare(b.created_at)),
      approved: all.filter((user) => user.status !== "pending"),
    };
  }, [users.data]);

  const columns: Column<ConsoleUser>[] = [
    { id: "email", header: "Email", cell: (user) => <EmailCell user={user} /> },
    {
      id: "status",
      header: "Status",
      className: "whitespace-nowrap",
      cell: (user) => <StatusBadge tone={userStatusTone(user.status)}>{user.status}</StatusBadge>,
    },
    {
      id: "role",
      header: "Role",
      className: "whitespace-nowrap text-zinc-400",
      cell: (user) => (user.admin ? "admin" : "operator"),
    },
    { id: "idps", header: "IdPs", cell: (user) => <IdpChips user={user} /> },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      className: "whitespace-nowrap",
      cell: (user) => (
        <div className="flex items-center justify-end gap-2">
          {user.admin && !user.current && (
            <ConfirmDialog
              title={`Make ${user.email} a user?`}
              confirmLabel="Make user"
              destructive={false}
              onConfirm={() => run(user, "demote")}
              trigger={
                <Button variant="secondary" size="sm" className={actionButton}>
                  Make user
                </Button>
              }
            />
          )}
          {!user.admin && (
            <Button
              variant="secondary"
              size="sm"
              className={actionButton}
              disabled={action.isPending}
              onClick={() => void run(user, "promote")}
            >
              Make admin
            </Button>
          )}
          {user.status === "active" && !user.current && (
            <ConfirmDialog
              title={`Disable ${user.email}?`}
              confirmLabel="Disable"
              onConfirm={() => run(user, "disable")}
              trigger={
                <Button variant="secondary" size="sm" className="hover:border-red-500/40 hover:text-red-300">
                  Disable
                </Button>
              }
            />
          )}
        </div>
      ),
    },
  ];

  return (
    <>
      <ControlTabs />
      <PageHeader title="Users" subtitle="Operator accounts. Approve pending sign-ins, change roles, or disable access." />

      {pending.length > 0 && (
        <section aria-labelledby="pending-users-heading" className="mb-10">
          <h2
            id="pending-users-heading"
            className="mb-3 text-xs font-semibold tracking-wider text-amber-300/80 uppercase light:text-amber-700"
          >
            {pluralize(pending.length, "pending approval")}
          </h2>
          <div className="overflow-hidden rounded-sm border border-amber-500/30 bg-amber-500/[0.04]">
            <table className="min-w-full text-sm">
              <caption className="sr-only">Pending users</caption>
              <tbody className="divide-y divide-border/70">
                {pending.map((user) => (
                  <tr key={user.id}>
                    <td className="px-5 py-2">
                      <EmailCell user={user} />
                    </td>
                    <td className="px-5 py-2">
                      <IdpChips user={user} />
                    </td>
                    <td className="px-5 py-2 text-subtle-foreground">
                      requested <LocalTime value={user.created_at} format="relative" />
                    </td>
                    <td className="px-5 py-2 text-right">
                      <Button size="sm" disabled={action.isPending} onClick={() => void run(user, "approve")}>
                        Approve
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      <DataTable
        caption="Users"
        columns={columns}
        rows={users.data ? approved : undefined}
        loading={users.isLoading}
        rowKey={(user) => user.id}
        empty={users.isError ? errorMessage(users.error) : "No approved users yet."}
      />
    </>
  );
}
