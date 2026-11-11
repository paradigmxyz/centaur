import { Search } from "lucide-react";
import { useState, type FormEvent } from "react";
import { useSearchParams } from "react-router";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

interface SearchBoxProps {
  placeholder: string;
  label: string;
  /** Query param name (default "q"). Submitting resets `page`. */
  param?: string;
}

/** GET-style search form bound to `?q=`, with Search and Clear. */
export function SearchBox({ placeholder, label, param = "q" }: SearchBoxProps) {
  const [params, setParams] = useSearchParams();
  const current = params.get(param) ?? "";
  const [value, setValue] = useState(current);
  const [synced, setSynced] = useState(current);
  if (synced !== current) {
    setSynced(current);
    setValue(current);
  }

  const apply = (next: string) =>
    setParams((prev) => {
      const copy = new URLSearchParams(prev);
      copy.delete("page");
      if (next.trim()) copy.set(param, next.trim());
      else copy.delete(param);
      return copy;
    });

  const onSubmit = (event: FormEvent) => {
    event.preventDefault();
    apply(value);
  };

  return (
    <form role="search" onSubmit={onSubmit} className="mb-3 flex flex-wrap items-center gap-2">
      <div className="relative w-full max-w-md">
        <span className="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-3 text-muted-foreground">
          <Search className="size-4" aria-hidden="true" />
        </span>
        <Input
          type="search"
          value={value}
          onChange={(event) => setValue(event.target.value)}
          placeholder={placeholder}
          aria-label={label}
          className="pl-9"
        />
      </div>
      <Button type="submit" variant="secondary">
        Search
      </Button>
      {current && (
        <button
          type="button"
          onClick={() => apply("")}
          className="cursor-pointer px-2 py-2 text-sm text-muted-foreground hover:text-centaur-300"
        >
          Clear
        </button>
      )}
    </form>
  );
}
