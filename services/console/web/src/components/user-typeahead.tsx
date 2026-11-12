import { useId, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

export interface UserOption {
  id: string;
  label: string;
  /** Lowercased text matched by the filter; defaults to the label. */
  searchText?: string;
}

interface UserTypeaheadProps {
  options: UserOption[];
  value: string[];
  onChange: (ids: string[]) => void;
  label: string;
  placeholder?: string;
  emptyText?: string;
}

export function UserTypeahead({
  options,
  value,
  onChange,
  label,
  placeholder = "Search people",
  emptyText = "No one selected.",
}: UserTypeaheadProps) {
  const listId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);

  const selected = useMemo(() => new Set(value), [value]);
  const byId = useMemo(() => new Map(options.map((option) => [option.id, option])), [options]);
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return options.filter(
      (option) => !selected.has(option.id) && (option.searchText ?? option.label.toLowerCase()).includes(q),
    );
  }, [options, query, selected]);

  const add = (option: UserOption | undefined) => {
    if (!option || selected.has(option.id)) return;
    onChange([...value, option.id]);
    setQuery("");
    setActive(-1);
    inputRef.current?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape") {
      setOpen(false);
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Enter"].includes(event.key) || visible.length === 0) return;
    event.preventDefault();
    setOpen(true);
    if (event.key === "Enter") {
      add(visible[active >= 0 ? active : 0]);
      return;
    }
    const offset = event.key === "ArrowDown" ? 1 : -1;
    setActive((current) =>
      current === -1 ? (offset === 1 ? 0 : visible.length - 1) : (current + offset + visible.length) % visible.length,
    );
  };

  return (
    <div
      className="space-y-2"
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setOpen(false);
      }}
    >
      <div className="relative">
        <Input
          ref={inputRef}
          role="combobox"
          aria-label={label}
          aria-expanded={open}
          aria-controls={listId}
          aria-activedescendant={active >= 0 ? `${listId}-${active}` : undefined}
          autoComplete="off"
          placeholder={placeholder}
          value={query}
          onFocus={() => setOpen(true)}
          onChange={(event) => {
            setQuery(event.target.value);
            setOpen(true);
            setActive(-1);
          }}
          onKeyDown={onKeyDown}
        />
        {open && visible.length > 0 && (
          <ul
            id={listId}
            role="listbox"
            className="absolute z-20 mt-1 max-h-60 w-full overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md"
          >
            {visible.map((option, index) => (
              <li key={option.id}>
                <button
                  type="button"
                  id={`${listId}-${index}`}
                  role="option"
                  aria-selected={index === active}
                  tabIndex={-1}
                  onPointerDown={(event) => event.preventDefault()}
                  onClick={() => add(option)}
                  className={cn(
                    "block w-full cursor-pointer rounded-sm px-2.5 py-1.5 text-left text-sm hover:bg-accent",
                    index === active && "bg-accent",
                  )}
                >
                  {option.label}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      {value.length === 0 ? (
        <p className="text-xs text-muted-foreground">{emptyText}</p>
      ) : (
        <div className="space-y-2">
          {value.map((id) => (
            <div key={id} className="flex items-center justify-between gap-3 rounded-lg border bg-muted/40 py-1.5 pr-1.5 pl-3">
              <span className="min-w-0 truncate text-sm">{byId.get(id)?.label ?? id}</span>
              <Button
                type="button"
                variant="ghost"
                size="xs"
                className="shrink-0 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                onClick={() => {
                  onChange(value.filter((other) => other !== id));
                  inputRef.current?.focus();
                  setOpen(true);
                }}
              >
                Remove
              </Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
