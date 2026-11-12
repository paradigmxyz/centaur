import { useQuery } from "@tanstack/react-query";
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";

import { Input } from "@/components/ui/input";
import { api } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";
import { cn } from "@/lib/utils";

export interface SlackChannelOption {
  channel_id: string;
  label: string;
  description: string;
  name?: string;
  private?: boolean;
}

export interface SlackChannelOptionsResult {
  options: SlackChannelOption[];
  error?: string | null;
}

/** Fetches picker options for a query. Must honour `signal`. */
export type SlackChannelLoader = (query: string, signal: AbortSignal) => Promise<SlackChannelOptionsResult>;

interface SlackChannelsResponse {
  data: SlackChannelOption[];
  meta: { catalog_error: string | null };
}

/**
 * Loader for admin pickers backed by GET /api/v1/console/slack_channels. Pass
 * "principal:<oid>" / "role:<oid>" to hide channels the owner already has.
 */
export function adminSlackChannelLoader(excludeOwner?: string): SlackChannelLoader {
  return async (query, signal) => {
    const body = await api.get<SlackChannelsResponse>(
      "/api/v1/console/slack_channels",
      { q: query, exclude_owner: excludeOwner },
      signal,
    );
    return { options: body.data, error: body.meta.catalog_error };
  };
}

export function useSlackChannelNames(channelIds: string[]) {
  const ids = [...new Set(channelIds)].sort();
  return useQuery({
    queryKey: sharedKeys.slackChannels({ ids }),
    queryFn: async ({ signal }) => {
      const body = await api.get<SlackChannelsResponse>("/api/v1/console/slack_channels", { ids: ids.length ? ids : ["-"] }, signal);
      return {
        names: Object.fromEntries(body.data.map((channel) => [channel.channel_id, channel.name ?? channel.label])),
        catalogError: body.meta.catalog_error,
      };
    },
  });
}

export function channelDisplay(label: string, channelId: string) {
  return `${label} (${channelId})`;
}

export const SLACK_DESTINATION_PATTERN = /^[CDGUW][A-Z0-9]{8,}$/;
const MAX_RESULTS = 20;

interface SlackChannelPickerProps {
  loadOptions: SlackChannelLoader;
  /**
   * The selected channel id, or "" when none. Initial display only: to clear
   * the input (e.g. after saving), remount the picker with a new `key`.
   */
  value: string;
  /**
   * Display label of the saved `value` (e.g. "#general"), so an edit form shows
   * it the way a fresh selection does. Without one the raw id is shown.
   */
  selectedLabel?: string | null;
  onChange: (channelId: string) => void;
  /** Reports false while the user typed text that is not a channel id. */
  onValidityChange?: (valid: boolean) => void;
  /** Manual-entry id format; defaults to channel, DM, group, or user ids. */
  pattern?: RegExp;
  id?: string;
  placeholder?: string;
  disabled?: boolean;
  "aria-label"?: string;
}

export function SlackChannelPicker({
  loadOptions,
  value,
  selectedLabel,
  onChange,
  onValidityChange,
  pattern = SLACK_DESTINATION_PATTERN,
  id,
  placeholder = "Search channels or enter an ID",
  disabled,
  "aria-label": ariaLabel,
}: SlackChannelPickerProps) {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const listId = `${inputId}-listbox`;
  const initialDisplay = value && selectedLabel ? channelDisplay(selectedLabel, value) : null;
  const [text, setText] = useState(initialDisplay ?? value);
  const [selectedDisplay, setSelectedDisplay] = useState<string | null>(initialDisplay);
  const [options, setOptions] = useState<SlackChannelOption[]>([]);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [status, setStatus] = useState<{ message: string; error: boolean }>({ message: "", error: false });
  const abortRef = useRef<AbortController | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);


  useEffect(
    () => () => {
      abortRef.current?.abort();
      if (timerRef.current) clearTimeout(timerRef.current);
    },
    [],
  );

  const search = async (query: string, preserving: boolean) => {
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    if (!preserving) setStatus({ message: "Loading channels…", error: false });
    try {
      const result = await loadOptions(query, controller.signal);
      setOptions(result.options);
      setActive(-1);
      if (result.error) setStatus({ message: result.error, error: false });
      else if (!preserving) setStatus({ message: resultStatus(result.options.length, query), error: false });
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return;
      setOptions([]);
      setStatus({ message: error instanceof Error ? error.message : "Could not load Slack channels.", error: true });
    }
  };

  const select = (option: SlackChannelOption) => {
    const display = channelDisplay(option.label, option.channel_id);
    setText(display);
    setSelectedDisplay(display);
    onChange(option.channel_id);
    onValidityChange?.(true);
    setStatus({ message: `Selected ${option.label}.`, error: false });
    setOpen(false);
    setActive(-1);
  };

  const onInput = (next: string) => {
    setText(next);
    setSelectedDisplay(null);
    setOpen(true);
    const upper = next.trim().toUpperCase();
    const manual = pattern.test(upper) ? upper : "";
    onChange(manual);
    onValidityChange?.(next.trim() === "" || manual !== "");
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => void search(next.trim(), false), 200);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (options.length === 0) return;
      const delta = event.key === "ArrowDown" ? 1 : -1;
      setOpen(true);
      setActive((current) => (current + delta + options.length) % options.length);
    } else if (event.key === "Enter" && active >= 0 && options[active]) {
      event.preventDefault();
      select(options[active]);
    } else if (event.key === "Escape") {
      setOpen(false);
    }
  };

  const visible = open && options.length > 0;
  return (
    <div className="relative">
      <Input
        id={inputId}
        role="combobox"
        aria-label={ariaLabel}
        aria-autocomplete="list"
        aria-controls={listId}
        aria-expanded={visible}
        aria-activedescendant={active >= 0 ? `${listId}-${active}` : undefined}
        autoComplete="off"
        placeholder={placeholder}
        disabled={disabled}
        value={text}
        onFocus={(event) => {
          setOpen(true);
          const preserving = text === selectedDisplay && selectedDisplay !== null;
          if (preserving) event.currentTarget.select();
          void search(preserving ? "" : text.trim(), preserving);
        }}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        onChange={(event) => onInput(event.target.value)}
        onKeyDown={onKeyDown}
      />
      <div
        id={listId}
        role="listbox"
        hidden={!visible}
        className="absolute z-20 mt-1 max-h-72 w-full overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md"
      >
        {options.map((option, index) => (
          <button
            key={option.channel_id}
            type="button"
            id={`${listId}-${index}`}
            role="option"
            aria-selected={index === active}
            tabIndex={-1}
            onPointerDown={(event) => event.preventDefault()}
            onClick={() => select(option)}
            className={cn(
              "block min-h-11 w-full cursor-pointer rounded-sm px-2.5 py-2 text-left transition-colors hover:bg-accent focus:outline-none",
              index === active && "bg-accent",
            )}
          >
            <div className="text-sm">{option.label}</div>
            <div className="mt-0.5 text-xs text-muted-foreground">{option.description}</div>
          </button>
        ))}
      </div>
      <div
        role="status"
        aria-live="polite"
        className={cn("mt-1.5 min-h-5 text-xs", status.error ? "text-destructive" : "text-muted-foreground")}
      >
        {status.message}
      </div>
    </div>
  );
}

function resultStatus(count: number, query: string): string {
  if (count === 0) return "No matching Slack destinations.";
  if (count === MAX_RESULTS) {
    return query === ""
      ? "Showing the first 20 channels. Type to search all channels."
      : "Showing the first 20 matching channels. Keep typing to narrow the results.";
  }
  return `${count} matching channel${count === 1 ? "" : "s"}.`;
}
