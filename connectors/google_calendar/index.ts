import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are Calendar's own resources, passed through (events, calendarList entries). Event
// times are Google's `{dateTime, timeZone}` for timed events and `{date}` for all-day ones. The
// reference is https://developers.google.com/calendar/api/v3/reference. Authentication is
// iron-proxy's oauth_token transform, which mints an access token from GOOGLE_TOKEN_JSON and
// injects the Authorization header on the wire; the connector itself sends no credentials unless
// GOOGLE_ACCESS_TOKEN is set (local testing only).

const CALENDAR = "https://www.googleapis.com/calendar/v3";

/** Event start or end: `dateTime` (RFC 3339) for timed events, `date` (YYYY-MM-DD) for all-day. */
export interface EventDateTime {
  date?: string;
  dateTime?: string;
  timeZone?: string;
  [field: string]: unknown;
}

/** One guest. `self` marks the calendar owner; `responseStatus` is accepted/declined/tentative/needsAction. */
export interface EventAttendee {
  email?: string;
  displayName?: string;
  responseStatus?: string;
  self?: boolean;
  organizer?: boolean;
  optional?: boolean;
  comment?: string;
  [field: string]: unknown;
}

/** Google Meet conference data; `entryPoints` carry the video URL once the conference exists. */
export interface ConferenceData {
  conferenceId?: string;
  entryPoints?: { entryPointType?: string; uri?: string; label?: string; [field: string]: unknown }[];
  createRequest?: { requestId?: string; status?: { statusCode?: string; [field: string]: unknown }; [field: string]: unknown };
  conferenceSolution?: { key?: { type?: string }; name?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/**
 * An event as Calendar holds it, and the body `create_event`/`update_event` write back. `id` is
 * assigned by Calendar, so it is optional here and required on `CalendarEvent`.
 */
export interface EventInput {
  id?: string;
  status?: string;
  htmlLink?: string;
  summary?: string;
  description?: string;
  location?: string;
  start?: EventDateTime;
  end?: EventDateTime;
  attendees?: EventAttendee[];
  organizer?: { email?: string; displayName?: string; self?: boolean; [field: string]: unknown };
  hangoutLink?: string;
  conferenceData?: ConferenceData;
  colorId?: string;
  visibility?: string;
  transparency?: string;
  recurrence?: string[];
  recurringEventId?: string;
  created?: string;
  updated?: string;
  iCalUID?: string;
  [field: string]: unknown;
}

/** A Calendar event as the API returns it. `start`/`end` are unset on cancelled instances; check `status`. */
export interface CalendarEvent extends EventInput {
  id: string;
}

/** One entry of the user's calendar list. */
export interface CalendarListEntry {
  id: string;
  summary?: string;
  description?: string;
  primary?: boolean;
  accessRole?: string;
  timeZone?: string;
  backgroundColor?: string;
  foregroundColor?: string;
  selected?: boolean;
  hidden?: boolean;
  summaryOverride?: string;
  [field: string]: unknown;
}

/** RSVP answer, as Calendar spells it. */
export type ResponseStatus = "accepted" | "declined" | "tentative";

const MEET_SOLUTION_KEY = "hangoutsMeet";
const DEFAULT_TIME_ZONE = "UTC";

function headers(ctx: Ctx): Record<string, string> {
  const out: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the access token is injected on the wire; locally it may come from the env.
  if (ctx.env.GOOGLE_ACCESS_TOKEN) out.authorization = `Bearer ${ctx.env.GOOGLE_ACCESS_TOKEN}`;
  return out;
}

/** A Calendar REST call; returns the JSON body, `{}` for an empty 204 (delete). */
async function calendar(
  ctx: Ctx,
  path: string,
  options: { method?: string; params?: Params; body?: unknown } = {},
): Promise<any> {
  const requestHeaders = headers(ctx);
  if (options.body !== undefined) requestHeaders["content-type"] = "application/json";
  const res = await ctx.fetch(`${CALENDAR}${path}${query(options.params ?? {})}`, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers: requestHeaders,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  if (res.status === 204) return {};
  return readJson(res, "Calendar");
}

const seg = (value: string): string => encodeURIComponent(value.trim());
const calPath = (calendarId: string): string => `/calendars/${seg(calendarId)}`;
const eventPath = (calendarId: string, eventId: string): string =>
  `${calPath(calendarId)}/events/${seg(eventId)}`;

function randomId(): string {
  let out = "";
  while (out.length < 32) out += Math.floor(Math.random() * 0x100000000).toString(16).padStart(8, "0");
  return out.slice(0, 32);
}

/** Ask Calendar to mint a Google Meet conference. `requestId` is the mint's idempotency key. */
function conferenceCreateRequest(): ConferenceData {
  return { createRequest: { requestId: randomId(), conferenceSolutionKey: { type: MEET_SOLUTION_KEY } } };
}

/** `dateTime` needs a time zone; a bare `YYYY-MM-DD` is an all-day event. */
function eventTime(value: string, timeZone: string): EventDateTime {
  return value.includes("T") ? { dateTime: value, timeZone } : { date: value };
}

/** The event's Google Meet URL, "" when it has none. */
function meetLink(event: CalendarEvent): string {
  if (event.hangoutLink) return event.hangoutLink;
  for (const entry of event.conferenceData?.entryPoints ?? []) {
    if (entry.entryPointType === "video" && entry.uri) return entry.uri;
  }
  return "";
}

/**
 * The Meet URL, re-fetching the event once when the mint is still pending: Calendar can answer the
 * write before the conference exists, leaving a createRequest and no entry points.
 */
async function awaitMeetLink(ctx: Ctx, calendarId: string, event: CalendarEvent): Promise<string> {
  const link = meetLink(event);
  if (link || !event.id) return link;
  return meetLink(await calendar(ctx, eventPath(calendarId, event.id), { params: { conferenceDataVersion: 1 } }));
}

/**
 * Whether an edit is one guests need to be told about: when, where, what it is called, who else is
 * coming, or a new call to join. Description-only edits are excluded on purpose. Field tests are
 * truthiness to match how `update_event` writes them.
 */
function isMaterialChange(change: {
  summary?: string;
  start?: string;
  end?: string;
  location?: string;
  attendeesAdded: boolean;
  conferenceAdded: boolean;
}): boolean {
  return (
    Boolean(change.summary || change.start || change.end || change.location) ||
    change.attendeesAdded ||
    change.conferenceAdded
  );
}

/**
 * The calendars on the user's list, with summary, primary flag, accessRole (owner, writer,
 * reader, freeBusyReader) and timeZone. Paginate with `next_page_token` as `page_token`;
 * `max_results` is at most 250.
 */
export async function list_calendars(
  args: {
    max_results?: number;
    page_token?: string;
    min_access_role?: string;
    show_hidden?: boolean;
    show_deleted?: boolean;
  },
  ctx: Ctx,
): Promise<{ calendars: CalendarListEntry[]; next_page_token: string | null }> {
  const body = await calendar(ctx, "/users/me/calendarList", {
    params: {
      maxResults: args.max_results,
      pageToken: args.page_token,
      minAccessRole: args.min_access_role,
      showHidden: args.show_hidden,
      showDeleted: args.show_deleted,
    },
  });
  return { calendars: body.items ?? [], next_page_token: body.nextPageToken ?? null };
}

/**
 * One calendar from the user's list by ID (`primary` is the default). This is the calendarList
 * entry: summary, accessRole, timeZone, colours, selection.
 */
export async function get_calendar(
  args: { calendar_id?: string },
  ctx: Ctx,
): Promise<{ calendar: CalendarListEntry }> {
  return {
    calendar: await calendar(ctx, `/users/me/calendarList/${seg(args.calendar_id ?? "primary")}`),
  };
}

/**
 * The calendar's IANA time zone (for example `America/Los_Angeles`), `UTC` when the calendar has
 * none. Use it to turn local dates into the RFC 3339 bounds `list_events` wants.
 */
export async function get_timezone(
  args: { calendar_id?: string },
  ctx: Ctx,
): Promise<{ time_zone: string }> {
  const entry: CalendarListEntry = await calendar(
    ctx,
    `/users/me/calendarList/${seg(args.calendar_id ?? "primary")}`,
  );
  return { time_zone: entry.timeZone || DEFAULT_TIME_ZONE };
}

/**
 * Events in a time range, expanded to single occurrences and ordered by start time. `time_min`
 * defaults to now; both bounds are RFC 3339 (`2024-01-15T00:00:00-08:00`); without an offset the
 * calendar's own time zone applies, so resolve it with `get_timezone`. `query` matches free text
 * (summary, description, location, guests). Follow `next_page_token` as `page_token` for more;
 * `max_results` defaults to 50 and is at most 2500. `order_by` defaults to `startTime`, which is
 * only valid with `single_events` (the default); Calendar allows `updated` only when single events
 * are off.
 */
export async function list_events(
  args: {
    calendar_id?: string;
    time_min?: string;
    time_max?: string;
    max_results?: number;
    query?: string;
    page_token?: string;
    single_events?: boolean;
    order_by?: "startTime" | "updated";
    time_zone?: string;
    show_deleted?: boolean;
    updated_min?: string;
  },
  ctx: Ctx,
): Promise<{ events: CalendarEvent[]; next_page_token: string | null }> {
  const body = await calendar(ctx, `${calPath(args.calendar_id ?? "primary")}/events`, {
    params: {
      timeMin: args.time_min ?? new Date().toISOString(),
      timeMax: args.time_max,
      maxResults: args.max_results ?? 50,
      q: args.query,
      pageToken: args.page_token,
      singleEvents: args.single_events ?? true,
      orderBy: args.order_by ?? ((args.single_events ?? true) ? "startTime" : undefined),
      timeZone: args.time_zone,
      showDeleted: args.show_deleted,
      updatedMin: args.updated_min,
    },
  });
  return { events: body.items ?? [], next_page_token: body.nextPageToken ?? null };
}

/**
 * One event in full: description, guests with their responseStatus, conference data, reminders,
 * recurrence and attachments. A cancelled event still returns with `status: "cancelled"`.
 */
export async function get_event(
  args: { event_id: string; calendar_id?: string },
  ctx: Ctx,
): Promise<{ event: CalendarEvent }> {
  return {
    event: await calendar(ctx, eventPath(args.calendar_id ?? "primary", args.event_id), {
      params: { conferenceDataVersion: 1 },
    }),
  };
}

/**
 * Create an event. `start`/`end` are RFC 3339 and are sent with `time_zone` (default UTC; give an
 * explicit offset in the time, or a matching zone, for local meetings), or `YYYY-MM-DD` for an
 * all-day event, where `end` is exclusive. `attendees` are emails and are emailed an invitation.
 * `conference: true` attaches a Google Meet; the returned `meet_link` is "" when there is none.
 */
export async function create_event(
  args: {
    summary: string;
    start: string;
    end: string;
    calendar_id?: string;
    description?: string;
    location?: string;
    attendees?: string[];
    conference?: boolean;
    time_zone?: string;
  },
  ctx: Ctx,
): Promise<{ event: CalendarEvent; meet_link: string }> {
  const calendarId = args.calendar_id ?? "primary";
  const zone = args.time_zone ?? DEFAULT_TIME_ZONE;
  const event: EventInput = {
    summary: args.summary,
    start: eventTime(args.start, zone),
    end: eventTime(args.end, zone),
  };
  if (args.description) event.description = args.description;
  if (args.location) event.location = args.location;
  if (args.attendees?.length) event.attendees = args.attendees.map((email) => ({ email }));

  const params: Params = { sendUpdates: args.attendees?.length ? "all" : "none" };
  if (args.conference) {
    event.conferenceData = conferenceCreateRequest();
    // Without conferenceDataVersion=1 Calendar drops conferenceData silently.
    params.conferenceDataVersion = 1;
  }

  const created: CalendarEvent = await calendar(ctx, `${calPath(calendarId)}/events`, {
    params,
    body: event,
  });
  return {
    event: created,
    meet_link: args.conference ? await awaitMeetLink(ctx, calendarId, created) : "",
  };
}

/**
 * Update an event by reading it and writing back the changed fields; unset arguments leave the
 * existing value alone (empty strings change nothing). `add_attendees` merges into the guest list
 * without duplicating existing guests. `conference: true` adds a Google Meet if the event has
 * none, and keeps an existing one. `notify` emails guests; by default it does so when the event
 * has guests and the change is material (time, title, location, new guests, new conference) —
 * a description-only edit stays quiet. Returns the updated event and its `meet_link` ("" when it
 * has none).
 */
export async function update_event(
  args: {
    event_id: string;
    calendar_id?: string;
    summary?: string;
    start?: string;
    end?: string;
    description?: string;
    location?: string;
    add_attendees?: string[];
    conference?: boolean;
    notify?: boolean;
    time_zone?: string;
  },
  ctx: Ctx,
): Promise<{ event: CalendarEvent; meet_link: string }> {
  const calendarId = args.calendar_id ?? "primary";
  const zone = args.time_zone ?? DEFAULT_TIME_ZONE;
  const event: EventInput = await calendar(ctx, eventPath(calendarId, args.event_id));

  if (args.summary) event.summary = args.summary;
  if (args.description) event.description = args.description;
  if (args.location) event.location = args.location;
  if (args.start) event.start = eventTime(args.start, zone);
  if (args.end) event.end = eventTime(args.end, zone);

  let attendeesAdded = false;
  if (args.add_attendees?.length) {
    const attendees = event.attendees ?? [];
    const known = new Set(attendees.map((attendee) => (attendee.email ?? "").toLowerCase()));
    for (const email of args.add_attendees) {
      if (known.has(email.toLowerCase())) continue;
      attendees.push({ email });
      known.add(email.toLowerCase());
      attendeesAdded = true;
    }
    event.attendees = attendees;
  }

  // conferenceDataVersion defaults to 0, under which Calendar ignores the conferenceData read
  // back above -- that is what keeps an existing Meet intact on an ordinary update.
  const params: Params = {};
  let conferenceAdded = false;
  if (args.conference) {
    params.conferenceDataVersion = 1;
    if (!event.conferenceData) {
      event.conferenceData = conferenceCreateRequest();
      conferenceAdded = true;
    }
  }

  const notify =
    args.notify ??
    (Boolean(event.attendees?.length) &&
      isMaterialChange({
        summary: args.summary,
        start: args.start,
        end: args.end,
        location: args.location,
        attendeesAdded,
        conferenceAdded,
      }));
  params.sendUpdates = notify ? "all" : "none";

  const updated: CalendarEvent = await calendar(ctx, eventPath(calendarId, args.event_id), {
    method: "PUT",
    params,
    body: event,
  });
  return {
    event: updated,
    meet_link: args.conference ? await awaitMeetLink(ctx, calendarId, updated) : "",
  };
}

/**
 * Delete (cancel) an event. `notify` defaults to true: guests are emailed that it was cancelled,
 * rather than leaving the meeting on their calendars. Returns the ID and deletion flag.
 */
export async function delete_event(
  args: { event_id: string; calendar_id?: string; notify?: boolean },
  ctx: Ctx,
): Promise<{ id: string; deleted: boolean }> {
  await calendar(ctx, eventPath(args.calendar_id ?? "primary", args.event_id), {
    method: "DELETE",
    params: { sendUpdates: (args.notify ?? true) ? "all" : "none" },
  });
  return { id: args.event_id, deleted: true };
}

/**
 * Answer an invite as the calendar owner: `response` is accepted, declined or tentative. Reads the
 * event, sets the owner's attendee responseStatus and notifies the organizer. Returns the updated
 * event and the status that was sent.
 */
export async function rsvp(
  args: { event_id: string; response: ResponseStatus; calendar_id?: string },
  ctx: Ctx,
): Promise<{ event: CalendarEvent; status: ResponseStatus }> {
  if (!["accepted", "declined", "tentative"].includes(args.response)) {
    throw new Error(`response must be accepted, declined or tentative, not ${JSON.stringify(args.response)}`);
  }
  const calendarId = args.calendar_id ?? "primary";
  const event: EventInput = await calendar(ctx, eventPath(calendarId, args.event_id));

  const mine = (event.attendees ?? []).find((attendee) => attendee.self);
  if (mine) mine.responseStatus = args.response;
  else event.attendees = [...(event.attendees ?? []), { email: "me", responseStatus: args.response, self: true }];

  const updated: CalendarEvent = await calendar(ctx, eventPath(calendarId, args.event_id), {
    method: "PUT",
    params: { sendUpdates: "all" },
    body: event,
  });
  return { event: updated, status: args.response };
}
