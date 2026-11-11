import { zodResolver } from "@hookform/resolvers/zod";
import { useState } from "react";
import { Controller, useForm, useWatch } from "react-hook-form";
import { Link } from "react-router";
import { z } from "zod";

import { ErrorAlert } from "@/components/alerts";
import { CheckboxField, SelectField, TextareaField, TextField } from "@/components/form/fields";
import { applyApiErrors } from "@/components/form/form-errors";
import { SlackChannelPicker } from "@/components/slack-channel-picker";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ApiError, errorMessage, humanize } from "@/lib/api";

import { deliveryChannelLoader, type ScheduledTask, type ScheduledTaskInput, type SchedulePreset } from "../api";

// ScheduledTask::SCHEDULE_PRESET_LABELS + Custom.
export const SCHEDULE_PRESET_OPTIONS: { value: SchedulePreset; label: string }[] = [
  { value: "manual", label: "Manual only" },
  { value: "daily", label: "Every day" },
  { value: "weekdays", label: "Every weekday" },
  { value: "mondays", label: "Mondays" },
  { value: "fridays", label: "Fridays" },
  { value: "custom", label: "Custom" },
];

// ScheduledTask::CUSTOM_SCHEDULE_DAYS (cron weekday values, Sunday last).
export const CUSTOM_DAYS: [string, string][] = [
  ["Mon", "1"],
  ["Tue", "2"],
  ["Wed", "3"],
  ["Thu", "4"],
  ["Fri", "5"],
  ["Sat", "6"],
  ["Sun", "0"],
];

// A delivery destination typed by hand must be a channel (the DM is its own mode).
const CHANNEL_ID_PATTERN = /^[CDG][A-Z0-9]{8,}$/;
const FIELDS = ["name", "prompt", "delivery_channel"] as const;

const schema = z
  .object({
    name: z.string().trim().min(1, "can't be blank").max(100, "is too long (maximum is 100 characters)"),
    prompt: z.string().trim().min(1, "can't be blank"),
    delivery_mode: z.enum(["dm", "channel"]),
    delivery_channel: z.string(),
    schedule_preset: z.enum(["manual", "daily", "weekdays", "mondays", "fridays", "custom"]),
    custom_days: z.array(z.string()),
    custom_time: z.string(),
    enabled: z.boolean(),
  })
  .refine((values) => values.schedule_preset !== "custom" || values.custom_time !== "", {
    path: ["custom_time"],
    message: "can't be blank",
  });

type Values = z.infer<typeof schema>;

interface ScheduledTaskFormProps {
  task: ScheduledTask;
  /** The task author is someone else (an admin editing another user's task). */
  foreignAuthor?: boolean;
  submitLabel: string;
  onSubmit: (data: ScheduledTaskInput) => Promise<unknown>;
}

/** The new/edit scheduled task form (legacy scheduled_tasks/_form). */
export function ScheduledTaskForm({ task, foreignAuthor = false, submitLabel, onSubmit }: ScheduledTaskFormProps) {
  const dmUserId = task.direct_message_user_id;
  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: {
      name: task.name ?? "",
      prompt: task.prompt ?? "",
      delivery_mode: task.delivery_mode === "dm" && !dmUserId ? "channel" : task.delivery_mode,
      delivery_channel: task.delivery_mode === "channel" ? (task.delivery_channel ?? "") : "",
      schedule_preset: task.schedule_preset,
      custom_days: task.custom_days,
      custom_time: task.custom_time,
      enabled: task.enabled,
    },
  });
  const [channelValid, setChannelValid] = useState(true);
  const [saveError, setSaveError] = useState<unknown>(null);
  const deliveryMode = useWatch({ control: form.control, name: "delivery_mode" });
  const schedulePreset = useWatch({ control: form.control, name: "schedule_preset" });
  const loadOptions = deliveryChannelLoader(task.id);

  const submit = form.handleSubmit(async (values) => {
    setSaveError(null);
    try {
      await onSubmit({
        ...values,
        delivery_channel: values.delivery_mode === "channel" ? values.delivery_channel : "",
        custom_days: values.schedule_preset === "custom" ? values.custom_days : [],
      });
    } catch (error) {
      setSaveError(error);
      applyApiErrors(form.setError, error, FIELDS);
    }
  });

  const messages = saveErrorMessages(saveError);
  const cronError = saveError instanceof ApiError && (saveError.fieldErrors.cron_expression?.length ?? 0) > 0;
  const customTimeError = form.formState.errors.custom_time;

  return (
    <form onSubmit={submit} className="space-y-5" noValidate>
      {messages.length > 0 && (
        <ErrorAlert className="mb-0">
          <p className="font-semibold">Could not save this task:</p>
          <ul className="mt-2 list-disc space-y-1 pl-5">
            {messages.map((message) => (
              <li key={message}>{message}</li>
            ))}
          </ul>
        </ErrorAlert>
      )}

      <TextField
        control={form.control}
        name="name"
        label="Name"
        placeholder="Daily support summary"
        hint="A label for finding and managing this task in the console."
      />

      <TextareaField
        control={form.control}
        name="prompt"
        label="Agent prompt"
        rows={12}
        placeholder="Review open support issues and summarize the most important changes."
        hint="Each run sends this prompt through one agent turn."
      />

      <fieldset>
        <legend className="mb-2 block text-xs font-semibold tracking-wider text-foreground uppercase">Deliver to</legend>
        <Controller
          control={form.control}
          name="delivery_mode"
          render={({ field }) => (
            <div className="flex flex-wrap gap-4">
              <label className="flex items-center gap-2 text-sm text-foreground">
                <input
                  type="radio"
                  name={field.name}
                  value="dm"
                  checked={field.value === "dm"}
                  disabled={!dmUserId}
                  onChange={() => field.onChange("dm")}
                  className="accent-centaur-500"
                />
                <span>{foreignAuthor ? "Deliver to the author's DM" : "Deliver to my DM"}</span>
              </label>
              <label className="flex items-center gap-2 text-sm text-foreground">
                <input
                  type="radio"
                  name={field.name}
                  value="channel"
                  checked={field.value === "channel"}
                  onChange={() => field.onChange("channel")}
                  className="accent-centaur-500"
                />
                <span>Deliver to a channel</span>
              </label>
            </div>
          )}
        />
        {deliveryMode === "channel" && (
          <div className="mt-3">
            <Controller
              control={form.control}
              name="delivery_channel"
              render={({ field, fieldState }) => (
                <>
                  <SlackChannelPicker
                    aria-label="Delivery channel"
                    loadOptions={loadOptions}
                    value={field.value}
                    onChange={field.onChange}
                    onValidityChange={setChannelValid}
                    pattern={CHANNEL_ID_PATTERN}
                  />
                  {fieldState.error && (
                    <p className="mt-1 text-xs text-red-400 light:text-red-700">{fieldState.error.message}</p>
                  )}
                </>
              )}
            />
            <p className="text-xs text-muted-foreground">
              Public channels are always available. Private channels require both you and the Centaur bot to be members.
            </p>
          </div>
        )}
        {!dmUserId && (
          <p className="mt-2 text-xs text-muted-foreground">
            {foreignAuthor
              ? "The author has not connected a Slack account, so tasks cannot deliver to their DM."
              : "Connect your Slack account to deliver to your DM."}
          </p>
        )}
      </fieldset>

      <div>
        <SelectField
          control={form.control}
          name="schedule_preset"
          label="Schedule"
          options={SCHEDULE_PRESET_OPTIONS}
          hint="Choose Manual only for an ad-hoc task, or run a preset at 9:00 AM Pacific Time."
        />

        {schedulePreset === "custom" && (
          <fieldset className="mt-4 rounded-sm border border-border bg-muted/40 p-4">
            <legend className="px-1 text-sm font-medium text-heading">Custom schedule</legend>
            <Controller
              control={form.control}
              name="custom_days"
              render={({ field }) => (
                <div role="group" aria-labelledby="custom-days-label">
                  <span id="custom-days-label" className="mb-2 block text-xs font-medium text-muted-foreground">
                    Days
                  </span>
                  <div className="grid grid-cols-4 gap-2 sm:grid-cols-7">
                    {CUSTOM_DAYS.map(([label, value]) => (
                      <label
                        key={value}
                        className="flex cursor-pointer items-center justify-center gap-2 rounded-sm border border-border bg-card px-2 py-2 text-sm text-foreground hover:border-primary/50"
                      >
                        <input
                          type="checkbox"
                          value={value}
                          checked={field.value.includes(value)}
                          onChange={(event) =>
                            field.onChange(
                              event.target.checked
                                ? [...field.value, value]
                                : field.value.filter((day) => day !== value),
                            )
                          }
                          className="accent-centaur-500"
                        />
                        <span>{label}</span>
                      </label>
                    ))}
                  </div>
                </div>
              )}
            />
            <div className="mt-4 max-w-xs">
              <label htmlFor="scheduled-task-custom-time" className="mb-2 block text-xs font-medium text-muted-foreground">
                Time
              </label>
              <Input
                id="scheduled-task-custom-time"
                type="time"
                required
                aria-invalid={customTimeError ? true : undefined}
                {...form.register("custom_time")}
              />
              {customTimeError && (
                <p className="mt-1 text-xs text-red-400 light:text-red-700">{customTimeError.message}</p>
              )}
              <p className="mt-2 text-xs text-muted-foreground">Custom schedules use Pacific Time.</p>
            </div>
          </fieldset>
        )}
        {cronError && (
          <p className="mt-2 text-xs text-red-400 light:text-red-700">Choose at least one day and a time.</p>
        )}
      </div>

      <div className="rounded-sm border border-border bg-muted/40 px-4 py-3">
        <CheckboxField
          control={form.control}
          name="enabled"
          label="Task enabled"
          hint="Disabled tasks remain saved but cannot run."
        />
      </div>

      <div className="flex flex-wrap items-center gap-3">
        <Button
          type="submit"
          disabled={form.formState.isSubmitting || (deliveryMode === "channel" && !channelValid)}
        >
          {submitLabel}
        </Button>
        <Button asChild variant="secondary">
          <Link to="/console/scheduled_tasks">Cancel</Link>
        </Button>
      </div>
    </form>
  );
}

/** Every server message as a full sentence ("Delivery channel is not available to the author"). */
function saveErrorMessages(error: unknown): string[] {
  if (!error) return [];
  if (error instanceof ApiError && error.status === 422 && Object.keys(error.fieldErrors).length > 0) {
    return Object.entries(error.fieldErrors).flatMap(([key, list]) =>
      list.map((message) => (key === "base" ? message : `${humanize(key)} ${message}`)),
    );
  }
  return [errorMessage(error)];
}
