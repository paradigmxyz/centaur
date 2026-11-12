import { X } from "lucide-react";
import { useState, type KeyboardEvent } from "react";

import { cn } from "@/lib/utils";

import { HTTP_METHODS, METHOD_WILDCARD, splitTerms } from "../form-model";

const chipClass = (on: boolean) =>
  cn(
    "h-7 cursor-pointer rounded-md border px-2.5 font-mono text-xs transition-[color,background-color,box-shadow] outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
    on
      ? "border-primary/50 bg-primary/15 text-primary"
      : "border-input bg-transparent text-muted-foreground hover:bg-accent hover:text-foreground dark:bg-input/30",
  );

/** Method toggles; ALL (`*`) and specific methods are mutually exclusive. */
export function MethodChips({
  value,
  onChange,
  label,
}: {
  value: string[];
  onChange: (methods: string[]) => void;
  label: string;
}) {
  const toggle = (method: string) => {
    const on = value.includes(method);
    if (method === METHOD_WILDCARD) onChange(on ? [] : [METHOD_WILDCARD]);
    else if (on) onChange(value.filter((m) => m !== method));
    else onChange([...value.filter((m) => m !== METHOD_WILDCARD), method]);
  };
  return (
    <div role="group" aria-label={label} className="flex flex-wrap gap-2">
      {[...HTTP_METHODS, METHOD_WILDCARD].map((method) => {
        const on = value.includes(method);
        return (
          <button
            key={method}
            type="button"
            aria-pressed={on}
            className={chipClass(on)}
            onClick={() => toggle(method)}
          >
            {method === METHOD_WILDCARD ? "ALL" : method}
          </button>
        );
      })}
    </div>
  );
}

export function PathChips({
  value,
  onChange,
  label,
}: {
  value: string[];
  onChange: (paths: string[]) => void;
  label: string;
}) {
  const [entry, setEntry] = useState("");

  const commit = (raw: string) => {
    const terms = splitTerms(raw).filter((term) => !value.includes(term));
    if (terms.length > 0) onChange([...value, ...terms]);
    setEntry("");
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      commit(entry);
    } else if (event.key === "Backspace" && entry === "" && value.length > 0) {
      onChange(value.slice(0, -1));
    }
  };

  return (
    <div className="flex min-h-9 flex-wrap items-center gap-1.5 rounded-md border border-input bg-transparent px-2 py-1.5 shadow-xs transition-[color,box-shadow] focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/50 dark:bg-input/30">
      {value.map((path) => (
        <span
          key={path}
          className="inline-flex items-center gap-1 rounded-md border border-primary/25 bg-primary/10 py-0.5 pr-1 pl-1.5 font-mono text-xs text-primary"
        >
          <span>{path}</span>
          <button
            type="button"
            aria-label={`Remove path ${path}`}
            className="cursor-pointer rounded-sm text-muted-foreground outline-none hover:text-destructive focus-visible:ring-2 focus-visible:ring-ring/50"
            onClick={() => onChange(value.filter((p) => p !== path))}
          >
            <X className="size-3" aria-hidden="true" />
          </button>
        </span>
      ))}
      <input
        type="text"
        aria-label={label}
        value={entry}
        onChange={(event) => {
          const next = event.target.value;
          if (next.includes(",")) commit(next);
          else setEntry(next);
        }}
        onKeyDown={onKeyDown}
        onBlur={() => commit(entry)}
        placeholder="/v1/* then comma"
        className="min-w-32 flex-1 bg-transparent font-mono text-xs text-foreground outline-none placeholder:text-muted-foreground"
      />
    </div>
  );
}
