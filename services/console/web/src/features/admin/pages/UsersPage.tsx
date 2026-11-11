import { useMemo } from "react";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { StatusBadge, userStatusTone } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCaption, TableCell, TableRow } from "@/components/ui/table";
import { errorMessage } from "@/lib/api";

import { useUserAction, useUsers, type ConsoleUser, type UserAction } from "../api";

const SUCCESS: Record<UserAction, (user: ConsoleUser) => string> = {
  approve: (user) => `Approved ${user.email}.`,
  disable: (user) => `Disabled ${user.email}.`,
  promote: (user) => `${user.email} is now an admin.`,
  demote: (user) => `${user.email} is now a user.`,
};

const actionButton = "hover:border-primary/40 hover:text-primary";

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
        <Badge key={label} variant="outline" className="rounded-md bg-muted/50 font-normal text-muted-foreground">
          {label.charAt(0).toUpperCase() + label.slice(1)}
        </Badge>
      ))}
    </div>
  );
}

function EmailCell({ user }: { user: ConsoleUser }) {
  return (
    <>
      <div className="font-medium">
        {user.email}
        {user.current && <span className="ml-2 text-xs text-muted-foreground">(you)</span>}
      </div>
      {user.name && <div className="text-xs text-muted-foreground">{user.name}</div>}
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
      className: "whitespace-nowrap text-muted-foreground",
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
                <Button variant="secondary" size="sm" className="hover:border-destructive/40 hover:text-destructive">
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

      {users.isError && <ErrorAlert>{errorMessage(users.error)}</ErrorAlert>}

      {pending.length > 0 && (
        <section aria-labelledby="pending-users-heading" className="mb-10">
          <h2
            id="pending-users-heading"
            className="mb-3 flex items-center gap-2 text-sm font-semibold text-warning"
          >
            {/* Fixed label, as on the legacy page. */}
            {pending.length} pending approval
          </h2>
          <div className="overflow-hidden rounded-xl border border-warning/30 bg-warning/5">
            <Table>
              <TableCaption className="sr-only">Pending users</TableCaption>
              <TableBody>
                {pending.map((user) => (
                  <TableRow key={user.id} className="border-warning/20 hover:bg-warning/5">
                    <TableCell className="px-5 py-2.5 whitespace-normal">
                      <EmailCell user={user} />
                    </TableCell>
                    <TableCell className="px-5 py-2.5">
                      <IdpChips user={user} />
                    </TableCell>
                    <TableCell className="px-5 py-2.5 text-muted-foreground">
                      requested <LocalTime value={user.created_at} format="relative" />
                    </TableCell>
                    <TableCell className="px-5 py-2.5 text-right">
                      <Button size="sm" disabled={action.isPending} onClick={() => void run(user, "approve")}>
                        Approve
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </section>
      )}

      {!users.isError && (
        <DataTable
          caption="Users"
          columns={columns}
          rows={users.data ? approved : undefined}
          loading={users.isLoading}
          rowKey={(user) => user.id}
          empty="No approved users yet."
        />
      )}
    </>
  );
}
