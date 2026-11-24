import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Zoom REST API v2 (api.zoom.us), acting as the Zoom user who connected the Console OAuth app.
// Results are Zoom's own meeting objects, passed through, except that `start_url` is removed: it
// starts the meeting as the host without signing in, so it must not reach chat or calendar
// invites. Authentication is the Console OAuth wrapper secret, which iron-proxy injects as a
// Bearer token on api.zoom.us; the connector sends no credentials unless ZOOM_ACCESS_TOKEN is set
// (local testing only). Reference: https://developers.zoom.us/docs/api/meetings/

const DEFAULT_API_BASE = "https://api.zoom.us/v2";

/** Meeting settings. Lists common fields; Zoom accepts and returns many more. */
export interface MeetingSettings {
  join_before_host?: boolean;
  waiting_room?: boolean;
  mute_upon_entry?: boolean;
  host_video?: boolean;
  participant_video?: boolean;
  /** `none`, `local` or `cloud`. */
  auto_recording?: string;
  /** Emails allowed to start the meeting as alternative hosts, separated by `;`. */
  alternative_hosts?: string;
  /** Email Zoom's own invitation to these people: `[{ email }]`. */
  meeting_invitees?: { email: string }[];
  [field: string]: unknown;
}

/** Recurrence for `type: 8` meetings. `type` is 1 daily, 2 weekly, 3 monthly. */
export interface MeetingRecurrence {
  type: number;
  repeat_interval?: number;
  /** Weekly days, comma-separated, 1 = Sunday through 7 = Saturday. */
  weekly_days?: string;
  monthly_day?: number;
  end_times?: number;
  end_date_time?: string;
  [field: string]: unknown;
}

/**
 * A Zoom meeting as the API returns it, without `start_url`. `id` is the numeric meeting ID people
 * type to join; `password` is the passcode; `join_url` is the link to share.
 */
export interface Meeting {
  id: number;
  uuid?: string;
  host_id?: string;
  host_email?: string;
  topic?: string;
  /** 1 instant, 2 scheduled, 3 recurring with no fixed time, 8 recurring with a fixed time. */
  type?: number;
  status?: string;
  start_time?: string;
  /** Minutes. */
  duration?: number;
  timezone?: string;
  agenda?: string;
  created_at?: string;
  join_url: string;
  password?: string;
  /** Passcode for joining by phone. */
  pstn_password?: string;
  settings?: MeetingSettings;
  recurrence?: MeetingRecurrence;
  occurrences?: { occurrence_id: string; start_time: string; duration?: number; status?: string }[];
  [field: string]: unknown;
}

/** One meeting in a `list_meetings` page. */
export interface MeetingSummary {
  id: number;
  uuid?: string;
  topic?: string;
  type?: number;
  start_time?: string;
  duration?: number;
  timezone?: string;
  agenda?: string;
  join_url?: string;
  [field: string]: unknown;
}

/** The connected Zoom user. */
export interface ZoomUser {
  id: string;
  email?: string;
  display_name?: string;
  first_name?: string;
  last_name?: string;
  timezone?: string;
  /** Personal meeting ID. */
  pmi?: number;
  personal_meeting_url?: string;
  [field: string]: unknown;
}

function apiBase(ctx: Ctx): string {
  return (ctx.env.ZOOM_API_BASE || DEFAULT_API_BASE).replace(/\/+$/, "");
}

/** A Zoom REST call; returns the JSON body, `{}` for an empty 204 (update, delete). */
async function zoom(
  ctx: Ctx,
  path: string,
  options: { method?: string; params?: Params; body?: unknown } = {},
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the OAuth token is injected on the wire; locally it may come from the env.
  if (ctx.env.ZOOM_ACCESS_TOKEN) headers.authorization = `Bearer ${ctx.env.ZOOM_ACCESS_TOKEN}`;
  if (options.body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(`${apiBase(ctx)}${path}${query(options.params ?? {})}`, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  if (res.status === 204) return {};
  return readJson(res, "Zoom");
}

/** Drop the host start link, which starts the meeting as the host without signing in. */
function withoutStartUrl<T extends Record<string, unknown>>(meeting: T): T {
  const { start_url: _startUrl, ...rest } = meeting;
  return rest as T;
}

function meetingPath(meetingId: number | string): string {
  const id = String(meetingId).replace(/\s+/g, "");
  if (!/^\d+$/.test(id)) throw new Error(`meeting_id must be a numeric Zoom meeting ID, not ${JSON.stringify(meetingId)}`);
  return `/meetings/${id}`;
}

/** The Zoom user whose account the connected OAuth credential acts as. */
export async function get_user(_args: Record<string, never>, ctx: Ctx): Promise<{ user: ZoomUser }> {
  return { user: await zoom(ctx, "/users/me") };
}

/**
 * Schedule a Zoom meeting as the connected user and return it with its `join_url`, numeric `id`
 * and `password` (passcode). `start_time` is `YYYY-MM-DDTHH:mm:ssZ` (UTC), or a local time without
 * an offset read in `timezone` (an IANA zone such as `America/New_York`; defaults to the user's
 * Zoom time zone). `duration` is in minutes (default 30). Zoom generates a passcode when the
 * account requires one; pass `password` to choose it. `type` defaults to 2 (scheduled); use 8 with
 * `recurrence` for a recurring meeting. `settings` is passed through, for example
 * `{ waiting_room: true }`.
 *
 * To put the meeting on a Google Calendar event as its video call, pass the returned meeting as
 * `zoom` to `google_calendar.create_event` or `google_calendar.update_event`.
 */
export async function create_meeting(
  args: {
    topic: string;
    start_time?: string;
    duration?: number;
    timezone?: string;
    agenda?: string;
    password?: string;
    type?: number;
    recurrence?: MeetingRecurrence;
    settings?: MeetingSettings;
  },
  ctx: Ctx,
): Promise<{ meeting: Meeting }> {
  const body: Record<string, unknown> = {
    topic: args.topic,
    type: args.type ?? 2,
    duration: args.duration ?? 30,
  };
  if (args.start_time) body.start_time = args.start_time;
  if (args.timezone) body.timezone = args.timezone;
  if (args.agenda) body.agenda = args.agenda;
  if (args.password) body.password = args.password;
  if (args.recurrence) body.recurrence = args.recurrence;
  if (args.settings) body.settings = args.settings;
  const meeting: Meeting = await zoom(ctx, "/users/me/meetings", { body });
  return { meeting: withoutStartUrl(meeting) };
}

/**
 * One meeting by its numeric ID, with join link, passcode, settings and, for recurring meetings,
 * `occurrences`. `occurrence_id` selects one occurrence of a recurring meeting.
 */
export async function get_meeting(
  args: { meeting_id: number | string; occurrence_id?: string },
  ctx: Ctx,
): Promise<{ meeting: Meeting }> {
  const meeting: Meeting = await zoom(ctx, meetingPath(args.meeting_id), {
    params: { occurrence_id: args.occurrence_id },
  });
  return { meeting: withoutStartUrl(meeting) };
}

/**
 * The connected user's meetings. `type` is `scheduled` (default), `live`, `upcoming`,
 * `upcoming_meetings` or `previous_meetings`; `from`/`to` (YYYY-MM-DD) bound the dates. Pass
 * `next_page_token` back as `page_token` for more; `page_size` defaults to 30, at most 300.
 */
export async function list_meetings(
  args: {
    type?: "scheduled" | "live" | "upcoming" | "upcoming_meetings" | "previous_meetings";
    page_size?: number;
    page_token?: string;
    from?: string;
    to?: string;
    timezone?: string;
  },
  ctx: Ctx,
): Promise<{ meetings: MeetingSummary[]; total_records: number | null; next_page_token: string | null }> {
  const body = await zoom(ctx, "/users/me/meetings", {
    params: {
      type: args.type ?? "scheduled",
      page_size: args.page_size ?? 30,
      next_page_token: args.page_token,
      from: args.from,
      to: args.to,
      timezone: args.timezone,
    },
  });
  return {
    meetings: (body.meetings ?? []).map(withoutStartUrl),
    total_records: body.total_records ?? null,
    next_page_token: body.next_page_token || null,
  };
}

/**
 * Change a meeting. Only the fields given are sent; `settings` merges into the existing settings.
 * `occurrence_id` changes one occurrence of a recurring meeting. Returns the meeting as it now is.
 * A calendar event that carries the meeting keeps the same join link and needs no change unless its
 * own time moves too.
 */
export async function update_meeting(
  args: {
    meeting_id: number | string;
    occurrence_id?: string;
    topic?: string;
    start_time?: string;
    duration?: number;
    timezone?: string;
    agenda?: string;
    password?: string;
    recurrence?: MeetingRecurrence;
    settings?: MeetingSettings;
  },
  ctx: Ctx,
): Promise<{ meeting: Meeting }> {
  const body: Record<string, unknown> = {};
  for (const key of ["topic", "start_time", "duration", "timezone", "agenda", "password", "recurrence", "settings"] as const) {
    if (args[key] !== undefined) body[key] = args[key];
  }
  if (Object.keys(body).length === 0) throw new Error("update_meeting needs at least one field to change");
  const path = meetingPath(args.meeting_id);
  await zoom(ctx, path, { method: "PATCH", params: { occurrence_id: args.occurrence_id }, body });
  const meeting: Meeting = await zoom(ctx, path, { params: { occurrence_id: args.occurrence_id } });
  return { meeting: withoutStartUrl(meeting) };
}

/**
 * Delete a meeting, or one occurrence with `occurrence_id`. `notify_host` and `notify_registrants`
 * default to false; a calendar event carrying the meeting is not changed, so delete or update it
 * separately. Returns the ID and deletion flag.
 */
export async function delete_meeting(
  args: {
    meeting_id: number | string;
    occurrence_id?: string;
    notify_host?: boolean;
    notify_registrants?: boolean;
  },
  ctx: Ctx,
): Promise<{ id: string; deleted: boolean }> {
  const path = meetingPath(args.meeting_id);
  await zoom(ctx, path, {
    method: "DELETE",
    params: {
      occurrence_id: args.occurrence_id,
      schedule_for_reminder: args.notify_host ?? false,
      cancel_meeting_reminder: args.notify_registrants ?? false,
    },
  });
  return { id: path.slice("/meetings/".length), deleted: true };
}
