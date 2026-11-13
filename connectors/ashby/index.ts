import type { Ctx } from "../centaur";

// Ashby's API is one POST endpoint per operation: a JSON body to
// `https://api.ashbyhq.com/<endpoint>` (for example `job.list`) answers
// `{ success, results, moreDataAvailable, nextCursor, errors }`. Results are
// Ashby's own objects, passed through; the interfaces list the fields worth
// knowing and Ashby sends more. List calls return a single page of at most 100
// objects: pass `next_cursor` back as `cursor` while `more_data_available`.

const DEFAULT_BASE = "https://api.ashbyhq.com";
const MAX_PAGE = 100;

interface Envelope {
  success?: boolean;
  results?: any;
  moreDataAvailable?: boolean;
  nextCursor?: string | null;
  errors?: unknown;
  errorInfo?: unknown;
  [field: string]: unknown;
}

/** A reference to another Ashby entity: `{ id, name }` for departments and locations, `{ id, title }` for sources, tags and stages. */
export interface Ref {
  id?: string;
  name?: string;
  title?: string;
  [field: string]: unknown;
}

export interface EmailAddress {
  value: string;
  /** Primary, Work, Personal, ... */
  type?: string;
  isPrimary?: boolean;
  [field: string]: unknown;
}

export interface PhoneNumber {
  value: string;
  /** Primary, Mobile, Home, Work, ... */
  type?: string;
  isPrimary?: boolean;
  [field: string]: unknown;
}

export interface SocialLink {
  /** LinkedIn, GitHub, Website, ... */
  type: string;
  url: string;
  [field: string]: unknown;
}

export interface FileHandle {
  /** "Resume" for an uploaded resume. */
  name?: string;
  handle: string;
  /** Ashby's file kind. */
  type?: string;
  [field: string]: unknown;
}

export interface Job {
  id: string;
  title: string;
  /** draft, open, closed or archived. */
  status?: string;
  confidential?: boolean;
  department?: Ref;
  location?: Ref;
  /** FullTime, PartTime, Intern, Contract, ... */
  employmentType?: string;
  createdAt?: string;
  updatedAt?: string;
  [field: string]: unknown;
}

export interface Candidate {
  id: string;
  name: string;
  /** Ashby web app URL for the candidate. */
  profileUrl?: string;
  primaryEmailAddress?: EmailAddress;
  emailAddresses?: EmailAddress[];
  phoneNumbers?: PhoneNumber[];
  /** Latest or current position, as entered on the candidate. */
  position?: string;
  /** Latest or current company. */
  company?: string;
  /** Latest school. */
  school?: string;
  socialLinks?: SocialLink[];
  fileHandles?: FileHandle[];
  tags?: Ref[];
  source?: Ref;
  applicationIds?: string[];
  createdAt?: string;
  updatedAt?: string;
  [field: string]: unknown;
}

export interface InterviewStage {
  id: string;
  /** Stage name. */
  title?: string;
  /** PreInterviewScreen, Active, Lead, Offer, ... */
  type?: string;
  /** Position of the stage in its interview plan, 0-based. */
  orderInInterviewPlan?: number;
  [field: string]: unknown;
}

export interface InterviewStageGroup {
  id: string;
  title?: string;
  orderInInterviewPlan?: number;
  [field: string]: unknown;
}

export interface Application {
  id: string;
  status?: string;
  candidate?: { id?: string; name?: string; [field: string]: unknown };
  job?: { id?: string; title?: string; [field: string]: unknown };
  currentInterviewStage?: InterviewStage;
  source?: Ref;
  isArchived?: boolean;
  createdAt?: string;
  updatedAt?: string;
  [field: string]: unknown;
}

/** One stage transition of an application. */
export interface StageHistoryEntry {
  id?: string;
  interviewStage?: InterviewStage;
  enteredAt?: string;
  exitedAt?: string | null;
  [field: string]: unknown;
}

export interface Interviewer {
  id?: string;
  firstName?: string;
  lastName?: string;
  email?: string;
  [field: string]: unknown;
}

export interface Interview {
  id: string;
  title?: string;
  interviewStage?: InterviewStage;
  interviewers?: Interviewer[];
  application?: {
    id?: string;
    candidate?: { id?: string; name?: string; [field: string]: unknown };
    [field: string]: unknown;
  };
  startTime?: string;
  endTime?: string;
  [field: string]: unknown;
}

export interface InterviewEvent {
  id: string;
  interview?: Interview;
  application?: Application;
  startTime?: string;
  endTime?: string;
  [field: string]: unknown;
}

export interface User {
  id: string;
  firstName?: string;
  lastName?: string;
  email?: string;
  role?: string;
  /** False for deactivated (former) employees. */
  isEnabled?: boolean;
  [field: string]: unknown;
}

export interface Department {
  id: string;
  name?: string;
  parent?: Ref;
  externalName?: string;
  isArchived?: boolean;
  [field: string]: unknown;
}

export interface Source {
  id: string;
  title?: string;
  isArchived?: boolean;
  [field: string]: unknown;
}

export interface CandidateTag {
  id: string;
  title?: string;
  [field: string]: unknown;
}

/** One interview scorecard submission. */
export interface Feedback {
  id: string;
  applicationId?: string;
  interview?: { id?: string; title?: string; [field: string]: unknown };
  submittedByUser?: User;
  submittedAt?: string;
  /** Attribute path -> submitted value, as the interview plan defines them. */
  submittedValues?: Record<string, unknown>;
  [field: string]: unknown;
}

export interface ApiKeyInfo {
  name?: string;
  isActive?: boolean;
  /** Module -> permissions. */
  permissions?: Record<string, unknown>;
  [field: string]: unknown;
}

interface Page<T> {
  items: T[];
  more_data_available: boolean;
  next_cursor: string | null;
}

/** An Ashby error body: `errors: [{ message }]` (or `errorInfo`), or a bare string. */
function message(value: unknown): string {
  if (Array.isArray(value)) return value.map(message).filter(Boolean).join("; ");
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    if (typeof record.message === "string" && record.message) return record.message;
    if (Array.isArray(record.errors)) {
      return record.errors.map(message).filter(Boolean).join("; ");
    }
    if (record.errorInfo) return message(record.errorInfo);
    return "";
  }
  return value === undefined || value === null ? "" : String(value);
}

async function request(
  ctx: Ctx,
  endpoint: string,
  body: Record<string, unknown>,
): Promise<Envelope> {
  const headers: Record<string, string> = {
    accept: "application/json; version=1",
    "content-type": "application/json",
  };
  // Behind iron-proxy the key is injected as Basic <base64(key:)> on the wire;
  // locally it comes from the environment.
  const key = ctx.env.ASHBY_API_KEY;
  if (key) headers.authorization = `Basic ${ctx.base64.encode(`${key}:`)}`;
  const res = await ctx.fetch(`${ctx.env.ASHBY_API_URL || DEFAULT_BASE}/${endpoint}`, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  const text = await res.text();
  let payload: Envelope | null = null;
  try {
    payload = text ? (JSON.parse(text) as Envelope) : {};
  } catch {
    payload = null;
  }
  if (!res.ok) {
    const detail = (payload && message(payload.errors)) || text.slice(0, 1000);
    throw new Error(`Ashby ${endpoint} returned HTTP ${res.status}: ${detail}`);
  }
  if (!payload) throw new Error(`Ashby ${endpoint} returned a non-JSON body: ${text.slice(0, 1000)}`);
  if (payload.success === false) {
    const detail = message(payload.errors) || message(payload.errorInfo) || "unknown error";
    throw new Error(`Ashby ${endpoint} failed: ${detail}`);
  }
  return payload;
}

/** One page of a `*.list` endpoint; `limit` is clamped to 1..100. */
async function page<T>(
  ctx: Ctx,
  endpoint: string,
  body: Record<string, unknown>,
  limit: number | undefined,
  cursor: string | undefined,
): Promise<Page<T>> {
  const size = Math.min(Math.max(Math.trunc(limit ?? MAX_PAGE), 1), MAX_PAGE);
  const result = await request(ctx, endpoint, { ...body, limit: size, cursor });
  return {
    items: Array.isArray(result.results) ? (result.results as T[]) : [],
    more_data_available: Boolean(result.moreDataAvailable),
    next_cursor: typeof result.nextCursor === "string" ? result.nextCursor : null,
  };
}

/** A single object from a `*.info` endpoint. */
async function one<T>(
  ctx: Ctx,
  endpoint: string,
  body: Record<string, unknown>,
  label: string,
): Promise<T> {
  const result = await request(ctx, endpoint, body);
  if (result.results === undefined || result.results === null) {
    throw new Error(`Ashby ${endpoint} returned no ${label} for ${JSON.stringify(body)}`);
  }
  return result.results as T;
}

/**
 * Jobs, one page. `status` keeps only that status (draft, open, closed, archived) and is applied to
 * the returned page, since Ashby cannot filter by it. Page with `cursor` while
 * `more_data_available`; `limit` is at most 100.
 */
export async function jobs(
  args: { status?: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ jobs: Job[]; more_data_available: boolean; next_cursor: string | null }> {
  const result = await page<Job>(ctx, "job.list", {}, args.limit, args.cursor);
  const status = args.status?.trim().toLowerCase();
  return {
    jobs: status ? result.items.filter((job) => job.status?.toLowerCase() === status) : result.items,
    more_data_available: result.more_data_available,
    next_cursor: result.next_cursor,
  };
}

/** One job by ID: title, status, department, location, employment type, timestamps. */
export async function job(args: { job_id: string }, ctx: Ctx): Promise<{ job: Job }> {
  return { job: await one<Job>(ctx, "job.info", { jobId: args.job_id }, "job") };
}

/**
 * Candidates, one page, with names, emails, phones, current position/company/school, social links,
 * file handles, tags and source. Page with `cursor`; `limit` is at most 100.
 */
export async function candidates(
  args: { limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ candidates: Candidate[]; more_data_available: boolean; next_cursor: string | null }> {
  const result = await page<Candidate>(ctx, "candidate.list", {}, args.limit, args.cursor);
  return {
    candidates: result.items,
    more_data_available: result.more_data_available,
    next_cursor: result.next_cursor,
  };
}

/**
 * One candidate by ID: contact details, background (position, company, school), social links,
 * file handles (resumes), tags, source, profile URL and application IDs.
 */
export async function candidate(
  args: { candidate_id: string },
  ctx: Ctx,
): Promise<{ candidate: Candidate }> {
  return {
    candidate: await one<Candidate>(ctx, "candidate.info", { id: args.candidate_id }, "candidate"),
  };
}

/**
 * Search candidates by name or email, up to `limit` results from one Ashby search. Ashby searches
 * either field, not both: a `query` containing "@" is searched as an email unless `by_email` is
 * false, and `by_email` forces the email field.
 */
export async function search_candidates(
  args: { query: string; limit?: number; by_email?: boolean },
  ctx: Ctx,
): Promise<{ candidates: Candidate[] }> {
  const byEmail = args.by_email ?? args.query.includes("@");
  const result = await request(
    ctx,
    "candidate.search",
    byEmail ? { email: args.query } : { name: args.query },
  );
  const found: Candidate[] = Array.isArray(result.results) ? result.results : [];
  const limit = Math.max(Math.trunc(args.limit ?? 20), 1);
  return { candidates: found.slice(0, limit) };
}

/**
 * Applications, one page, optionally for one job. Each carries candidate and job references, status,
 * current interview stage and archive flag. Page with `cursor`; `limit` is at most 100.
 */
export async function applications(
  args: { job_id?: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{
  applications: Application[];
  more_data_available: boolean;
  next_cursor: string | null;
}> {
  const result = await page<Application>(
    ctx,
    "application.list",
    args.job_id ? { jobId: args.job_id } : {},
    args.limit,
    args.cursor,
  );
  return {
    applications: result.items,
    more_data_available: result.more_data_available,
    next_cursor: result.next_cursor,
  };
}

/** One application by ID: candidate, job, status, current stage, source, timestamps. */
export async function application(
  args: { application_id: string },
  ctx: Ctx,
): Promise<{ application: Application }> {
  return {
    application: await one<Application>(
      ctx,
      "application.info",
      { applicationId: args.application_id },
      "application",
    ),
  };
}

/**
 * An application's stage history, oldest first: which interview stage it entered, when it entered and
 * when it left (`exited_at` null for the current stage).
 */
export async function application_history(
  args: { application_id: string },
  ctx: Ctx,
): Promise<{ history: StageHistoryEntry[] }> {
  const result = await request(ctx, "application.listHistory", {
    applicationId: args.application_id,
  });
  return { history: Array.isArray(result.results) ? result.results : [] };
}

/**
 * Scheduled interviews, one page, with stage, interviewers, application/candidate and start/end time.
 * Filter by `startTime` yourself if you need only upcoming ones. Page with `cursor`; `limit` at most 100.
 */
export async function interviews(
  args: { limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ interviews: Interview[]; more_data_available: boolean; next_cursor: string | null }> {
  const result = await page<Interview>(ctx, "interview.list", {}, args.limit, args.cursor);
  return {
    interviews: result.items,
    more_data_available: result.more_data_available,
    next_cursor: result.next_cursor,
  };
}

/** One interview by ID: stage, interviewers, application/candidate, start and end time. */
export async function interview(
  args: { interview_id: string },
  ctx: Ctx,
): Promise<{ interview: Interview }> {
  return {
    interview: await one<Interview>(ctx, "interview.info", { interviewId: args.interview_id }, "interview"),
  };
}

/** Every interview stage defined in the workspace, with name, type and order in the interview plan. */
export async function interview_stages(args: {}, ctx: Ctx): Promise<{ stages: InterviewStage[] }> {
  const result = await request(ctx, "interviewStage.list", {});
  return { stages: Array.isArray(result.results) ? result.results : [] };
}

/** Every interview stage group (grouped stages of an interview plan), with name and order. */
export async function interview_stage_groups(
  args: {},
  ctx: Ctx,
): Promise<{ groups: InterviewStageGroup[] }> {
  const result = await request(ctx, "interviewStageGroup.list", {});
  return { groups: Array.isArray(result.results) ? result.results : [] };
}

/**
 * Workspace users, one page: first/last name, email, role, and `is_enabled` (false for former
 * employees). Deactivated users are excluded unless `include_disabled` is set. Page with `cursor`;
 * `limit` at most 100.
 */
export async function users(
  args: { limit?: number; cursor?: string; include_disabled?: boolean },
  ctx: Ctx,
): Promise<{ users: User[]; more_data_available: boolean; next_cursor: string | null }> {
  const body: Record<string, unknown> = args.include_disabled ? { includeDeactivated: true } : {};
  const result = await page<User>(ctx, "user.list", body, args.limit, args.cursor);
  return {
    users: args.include_disabled ? result.items : result.items.filter((user) => user.isEnabled ?? true),
    more_data_available: result.more_data_available,
    next_cursor: result.next_cursor,
  };
}

/** One user by ID: name, email, role, enabled flag. */
export async function user(args: { user_id: string }, ctx: Ctx): Promise<{ user: User }> {
  return { user: await one<User>(ctx, "user.info", { userId: args.user_id }, "user") };
}

/** Info about the API key in use: name, active flag and per-module permissions. Useful as an auth check. */
export async function api_key_info(args: {}, ctx: Ctx): Promise<{ api_key: ApiKeyInfo }> {
  const result = await request(ctx, "apiKey.info", {});
  return { api_key: (result.results ?? {}) as ApiKeyInfo };
}

/** Departments, with name, parent and archive flag. */
export async function departments(args: {}, ctx: Ctx): Promise<{ departments: Department[] }> {
  const result = await request(ctx, "department.list", {});
  return { departments: Array.isArray(result.results) ? result.results : [] };
}

/** Candidate sources (where applications came from), with title and archive flag. */
export async function sources(args: {}, ctx: Ctx): Promise<{ sources: Source[] }> {
  const result = await request(ctx, "source.list", {});
  return { sources: Array.isArray(result.results) ? result.results : [] };
}

/** Candidate tags defined in the workspace, with title. */
export async function candidate_tags(args: {}, ctx: Ctx): Promise<{ tags: CandidateTag[] }> {
  const result = await request(ctx, "candidateTag.list", {});
  return { tags: Array.isArray(result.results) ? result.results : [] };
}

/**
 * Download URL for a file handle (a resume, for example). The URL is short-lived; `url` is null when
 * Ashby returns none. Building the URL is all a connector can do — downloading the bytes is the
 * caller's job.
 */
export async function file_url(
  args: { file_handle: string },
  ctx: Ctx,
): Promise<{ url: string | null }> {
  const result = await one<{ url?: string }>(
    ctx,
    "file.info",
    { fileHandle: args.file_handle },
    "file",
  );
  return { url: result.url ?? null };
}

/**
 * Download URL for a candidate's resume: the first file handle whose type is "Resume" or whose name
 * contains "resume", resolved through `file_url`. `url` is null when the candidate has no resume.
 */
export async function resume_url(
  args: { candidate_id: string },
  ctx: Ctx,
): Promise<{ url: string | null }> {
  const found = await one<Candidate>(ctx, "candidate.info", { id: args.candidate_id }, "candidate");
  const handles: FileHandle[] = Array.isArray(found.fileHandles) ? found.fileHandles : [];
  const resume = handles.find(
    (handle) => handle.type === "Resume" || (handle.name ?? "").toLowerCase().includes("resume"),
  );
  if (!resume?.handle) return { url: null };
  const file = await one<{ url?: string }>(
    ctx,
    "file.info",
    { fileHandle: resume.handle },
    "file",
  );
  return { url: file.url ?? null };
}

/**
 * Interview scorecards, one page, optionally for one application: interviewer, interview, submission
 * time, and the submitted values keyed by the interview plan's attribute paths. Scores are not
 * normalized — read `submittedValues`. Page with `cursor`; `limit` at most 100.
 */
export async function application_feedback(
  args: { application_id?: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ feedback: Feedback[]; more_data_available: boolean; next_cursor: string | null }> {
  const result = await page<Feedback>(
    ctx,
    "applicationFeedback.list",
    args.application_id ? { applicationId: args.application_id } : {},
    args.limit,
    args.cursor,
  );
  return {
    feedback: result.items,
    more_data_available: result.more_data_available,
    next_cursor: result.next_cursor,
  };
}

/**
 * Interview events (scheduled interview sessions with start/end times), one page. Page with `cursor`;
 * `limit` at most 100.
 */
export async function interview_events(
  args: { limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ events: InterviewEvent[]; more_data_available: boolean; next_cursor: string | null }> {
  const result = await page<InterviewEvent>(ctx, "interviewEvent.list", {}, args.limit, args.cursor);
  return {
    events: result.items,
    more_data_available: result.more_data_available,
    next_cursor: result.next_cursor,
  };
}
