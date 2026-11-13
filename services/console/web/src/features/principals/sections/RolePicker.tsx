import { ChevronsUpDown } from "lucide-react";
import { useDeferredValue, useState } from "react";

import { Button } from "@/components/ui/button";
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { errorMessage } from "@/lib/api";
import { displayLabel } from "@/lib/format";

import { useAssignableRoles, type RoleSummary } from "../api";

interface RolePickerProps {
  principalId: string;
  value: RoleSummary | null;
  onChange: (role: RoleSummary) => void;
}

/** Searches server-side with `exclude_principal`, so roles beyond the first API page stay reachable. */
export function RolePicker({ principalId, value, onChange }: RolePickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const deferred = useDeferredValue(query.trim());
  const roles = useAssignableRoles(principalId, deferred, open);
  const rows = roles.data?.data ?? [];
  const total = roles.data?.meta.total ?? 0;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="secondary"
          role="combobox"
          aria-expanded={open}
          aria-label="Role to assign"
          className="w-72 max-w-full justify-between py-2 font-normal"
        >
          <span className="truncate">{value ? displayLabel(value) : "Pick a role to assign"}</span>
          <ChevronsUpDown className="opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-[min(24rem,90vw)] p-0" align="start">
        <Command shouldFilter={false}>
          <CommandInput placeholder="Search roles" value={query} onValueChange={setQuery} />
          <CommandList>
            {roles.isError ? (
              <p role="alert" className="px-3 py-4 text-sm text-destructive">
                Could not load roles: {errorMessage(roles.error)}
              </p>
            ) : (
              <CommandEmpty>{roles.isLoading ? "Loading…" : "No roles found."}</CommandEmpty>
            )}
            {!roles.isError && rows.length > 0 && (
              <CommandGroup>
                {rows.map((role) => (
                  <CommandItem
                    key={role.id}
                    value={role.id}
                    onSelect={() => {
                      onChange(role);
                      setOpen(false);
                    }}
                  >
                    {displayLabel(role)}
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
            {!roles.isError && total > rows.length && (
              <p className="px-3 py-2 text-xs text-muted-foreground">
                Showing {rows.length} of {total} roles. Search to find the rest.
              </p>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
