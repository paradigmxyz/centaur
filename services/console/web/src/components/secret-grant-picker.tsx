import { useQuery } from "@tanstack/react-query";
import { ChevronsUpDown } from "lucide-react";
import { useDeferredValue, useState } from "react";

import { Button } from "@/components/ui/button";
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { api, errorMessage, type ListEnvelope } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";
import { SECRET_KIND_SLUGS, secretKindLabel, secretOptionLabel, type GrantableSummary } from "@/lib/secret-kinds";

interface SecretGrantPickerProps {
  value: GrantableSummary | null;
  onChange: (secret: GrantableSummary | null) => void;
  /** Secret ids to leave out (e.g. already granted directly). */
  excludeIds?: ReadonlySet<string>;
  /**
   * `principal:<oid>` or `role:<oid>`: the server leaves out every secret
   * granted directly to that grantee (all pages, not just loaded ones).
   */
  excludeGrantee?: string;
  placeholder?: string;
}

/** Shows identifying metadata only. */
export function SecretGrantPicker({
  value,
  onChange,
  excludeIds,
  excludeGrantee,
  placeholder = "Pick a secret to grant",
}: SecretGrantPickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const deferred = useDeferredValue(query.trim());
  const params = { q: deferred, limit: 200, exclude_grantee: excludeGrantee };
  const secrets = useQuery({
    queryKey: sharedKeys.secrets(params),
    queryFn: ({ signal }) => api.get<ListEnvelope<GrantableSummary>>("/api/v1/secrets", params, signal),
    enabled: open,
    select: (body) => body.data.filter((secret) => !excludeIds?.has(secret.id)),
  });

  const groups = SECRET_KIND_SLUGS.map((kind) => ({
    kind,
    items: (secrets.data ?? []).filter((secret) => secret.kind === kind),
  })).filter((group) => group.items.length > 0);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          aria-label="Secret to grant"
          className="w-full max-w-md justify-between py-2 font-normal"
        >
          <span className="truncate">{value ? `${value.kind_label}: ${secretOptionLabel(value)}` : placeholder}</span>
          <ChevronsUpDown className="opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-[min(28rem,90vw)] p-0" align="start">
        <Command shouldFilter={false}>
          <CommandInput placeholder="Search secrets" value={query} onValueChange={setQuery} />
          <CommandList>
            {secrets.isError ? (
              <p role="alert" className="px-3 py-4 text-sm text-destructive">
                Could not load secrets: {errorMessage(secrets.error)}
              </p>
            ) : (
              <CommandEmpty>{secrets.isLoading ? "Loading…" : "No secrets found."}</CommandEmpty>
            )}
            {!secrets.isError && groups.map((group) => (
              <CommandGroup key={group.kind} heading={secretKindLabel(group.kind)}>
                {group.items.map((secret) => (
                  <CommandItem
                    key={secret.id}
                    value={`${secret.kind}:${secret.id}`}
                    onSelect={() => {
                      onChange(secret);
                      setOpen(false);
                    }}
                  >
                    {secretOptionLabel(secret)}
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
