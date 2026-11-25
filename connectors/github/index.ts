import type { Ctx } from "../centaur";
import type { Params } from "../_lib/http";
import { query } from "../_lib/http";

// GitHub REST API (api.github.com). Covers the gh CLI's remote commands (search,
// pr, issue, run, release, api) and leaves out everything that needs a local
// checkout or files (clone, checkout, push, uploads, downloads). Responses are
// GitHub's own objects, passed through unchanged; `request` reaches any endpoint
// not wrapped here. Behind iron-proxy, the GITHUB_TOKEN placeholder in the
// environment is swapped for the real token on the wire.

const DEFAULT_BASE = "https://api.github.com";
const API_VERSION = "2022-11-28";

/** A GitHub user or organization, as embedded in other objects. */
export interface GitHubUser {
  login: string;
  id?: number;
  type?: string;
  html_url?: string;
  [field: string]: unknown;
}

/** A label on an issue or pull request. */
export interface GitHubLabel {
  name: string;
  color?: string;
  description?: string | null;
  [field: string]: unknown;
}

function base(ctx: Ctx): string {
  return (ctx.env.GITHUB_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx, accept: string | undefined): Record<string, string> {
  const value: Record<string, string> = {
    accept: accept ?? "application/vnd.github+json",
    "x-github-api-version": API_VERSION,
    // api.github.com rejects requests without a User-Agent.
    "user-agent": "centaur-codemode",
  };
  // The placeholder (or, locally, a real token); iron-proxy replaces it on the wire.
  if (ctx.env.GITHUB_TOKEN) value.authorization = `Bearer ${ctx.env.GITHUB_TOKEN}`;
  return value;
}

/** `owner/name`, validated and URL-encoded per segment. */
function repoPath(repo: string): string {
  const parts = (repo || "").trim().replace(/^\/+|\/+$/g, "").split("/");
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    throw new Error(`repo must be "owner/name", got ${JSON.stringify(repo)}`);
  }
  return `/repos/${encodeURIComponent(parts[0])}/${encodeURIComponent(parts[1])}`;
}

/** A repository file path, URL-encoded per segment with the slashes kept. */
function filePath(path: string): string {
  return (path || "")
    .replace(/^\/+|\/+$/g, "")
    .split("/")
    .map(encodeURIComponent)
    .join("/");
}

/** A required, trimmed string argument. */
function required(value: string | undefined, label: string): string {
  const text = (value || "").trim();
  if (!text) throw new Error(`${label} is required`);
  return text;
}

/** A positive integer path segment (a pull request, issue, run or job number). */
function positive(value: number | string, label: string): string {
  const text = String(value ?? "").trim();
  if (!/^[1-9]\d*$/.test(text)) throw new Error(`${label} must be a positive number, got ${JSON.stringify(value)}`);
  return text;
}

/** Only the keys that are set, so a PATCH leaves everything else alone. */
function defined(fields: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(fields).filter(([, value]) => value !== undefined));
}

/** UTF-8 size and UTF-16 width of the code point ending (`fromEnd`) or starting at `index`. */
function codePoint(text: string, index: number, fromEnd: boolean): { size: number; width: number } {
  const unit = text.charCodeAt(index);
  const pair = fromEnd
    ? unit >= 0xdc00 && unit <= 0xdfff && index > 0 && text.charCodeAt(index - 1) >= 0xd800 && text.charCodeAt(index - 1) <= 0xdbff
    : unit >= 0xd800 && unit <= 0xdbff && index + 1 < text.length && text.charCodeAt(index + 1) >= 0xdc00 && text.charCodeAt(index + 1) <= 0xdfff;
  if (pair) return { size: 4, width: 2 };
  return { size: unit < 0x80 ? 1 : unit < 0x800 ? 2 : 3, width: 1 };
}

/**
 * At most `maxBytes` UTF-8 bytes of `text`, from the start or (`fromEnd`) the end, never
 * splitting a character. `bytes` is the size of the returned text.
 */
function clip(text: string, maxBytes: number, fromEnd: boolean): { text: string; bytes: number; truncated: boolean } {
  const limit = Math.max(0, Math.floor(Number.isFinite(maxBytes) ? maxBytes : 0));
  let bytes = 0;
  if (fromEnd) {
    let index = text.length;
    while (index > 0) {
      const { size, width } = codePoint(text, index - 1, true);
      if (bytes + size > limit) break;
      bytes += size;
      index -= width;
    }
    return { text: text.slice(index), bytes, truncated: index > 0 };
  }
  let index = 0;
  while (index < text.length) {
    const { size, width } = codePoint(text, index, false);
    if (bytes + size > limit) break;
    bytes += size;
    index += width;
  }
  return { text: text.slice(0, index), bytes, truncated: index < text.length };
}

/** Only GitHub API paths, never another host or an absolute URL. */
function endpoint(path: string): string {
  let value = (path || "").trim();
  if (!value) throw new Error("path is required");
  if (value.includes("://") || value.startsWith("//")) {
    throw new Error("path must be a GitHub API path such as /repos/owner/name, not a URL");
  }
  if (!value.startsWith("/")) value = `/${value}`;
  return value;
}

/** GitHub errors carry `message` and sometimes `errors`; fall back to the raw body. */
function errorMessage(text: string): string {
  try {
    const payload = JSON.parse(text);
    if (payload && typeof payload.message === "string") {
      const details = Array.isArray(payload.errors)
        ? payload.errors
            .map((error: any) => (typeof error === "string" ? error : error?.message || error?.code))
            .filter(Boolean)
            .join("; ")
        : "";
      return details ? `${payload.message}: ${details}` : payload.message;
    }
  } catch {
    // Not JSON; use the body as is.
  }
  return text.trim().slice(0, 1000);
}

interface CallOptions {
  params?: Params;
  body?: unknown;
  /** Accept header, for example `application/vnd.github.diff`. */
  accept?: string;
  /** Return the body as text instead of parsing JSON. */
  text?: boolean;
}

/**
 * Call the GitHub API. Returns parsed JSON (`{}` for an empty or 204 body), or the body as a
 * string with `text: true`. Throws `GitHub API error (<status>): <message>` on 4xx/5xx.
 * Rate limits (429, or 403 with `x-ratelimit-remaining: 0`) are retried twice after a short wait.
 */
async function gh(ctx: Ctx, method: string, path: string, options: CallOptions = {}): Promise<any> {
  const url = `${base(ctx)}${endpoint(path)}${query(options.params ?? {})}`;
  const init = {
    method: method.toUpperCase(),
    headers: {
      ...headers(ctx, options.accept),
      ...(options.body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  };
  let res = await ctx.fetch(url, init);
  for (
    let attempt = 0;
    attempt < 2 &&
    (res.status === 429 || (res.status === 403 && res.headers["x-ratelimit-remaining"] === "0"));
    attempt += 1
  ) {
    const retry = Number(res.headers["retry-after"] || "2");
    await ctx.sleep(Math.min(Number.isFinite(retry) ? retry : 2, 10) * 1000);
    res = await ctx.fetch(url, init);
  }
  const text = await res.text();
  if (res.status >= 400) throw new Error(`GitHub API error (${res.status}): ${errorMessage(text)}`);
  if (options.text) return text;
  if (res.status === 204 || !text) return {};
  return JSON.parse(text);
}

/**
 * Call any GitHub REST endpoint, like `gh api`. `path` is an API path such as
 * `/repos/owner/name/labels` (no host); `method` defaults to GET; `params` become the query
 * string and `body` is sent as JSON (for example `{ method: "PATCH", path, body }`). `accept`
 * overrides the Accept header (such as `application/vnd.github.raw+json`), and `text: true`
 * returns the body as a string. Returns `{ data }`, with `{}` for an empty response.
 */
export async function request(
  args: {
    path: string;
    method?: string;
    params?: Params;
    body?: unknown;
    accept?: string;
    text?: boolean;
  },
  ctx: Ctx,
): Promise<{ data: any }> {
  const data = await gh(ctx, args.method ?? "GET", args.path, {
    params: args.params,
    body: args.body,
    accept: args.accept,
    text: args.text,
  });
  return { data };
}

/** The authenticated user (or app bot) the token belongs to, as `{ user }`. */
export async function get_viewer(_args: Record<string, never>, ctx: Ctx): Promise<{ user: GitHubUser }> {
  return { user: await gh(ctx, "GET", "/user") };
}


// ---- search_repos ----

/** One issue or pull request from `/search/issues`. */
export interface SearchIssueItem {
  id: number;
  number?: number;
  title?: string;
  state?: string;
  /** `open`, `closed`, or the state reason. */
  state_reason?: string | null;
  body?: string | null;
  html_url?: string;
  url?: string;
  created_at?: string;
  updated_at?: string;
  closed_at?: string | null;
  comments?: number;
  draft?: boolean;
  user?: GitHubUser;
  assignee?: GitHubUser | null;
  assignees?: GitHubUser[];
  labels?: GitHubLabel[];
  milestone?: { title?: string; number?: number; [field: string]: unknown } | null;
  /** The repository API URL, for example `https://api.github.com/repos/owner/name`. */
  repository_url?: string;
  /** Present only on pull requests. */
  pull_request?: { url?: string; html_url?: string; merged_at?: string | null; [field: string]: unknown };
  /** Relevance score; only search results carry it. */
  score?: number;
  [field: string]: unknown;
}

/** GitHub's issue search envelope; `items` is empty when nothing matched. */
export interface SearchIssueList {
  total_count?: number;
  incomplete_results?: boolean;
  items?: SearchIssueItem[];
  [field: string]: unknown;
}

/** One file hit from `/search/code`. */
export interface SearchCodeItem {
  name?: string;
  path?: string;
  sha?: string;
  html_url?: string;
  url?: string;
  git_url?: string;
  score?: number;
  /** The repository the file lives in, as GitHub embeds it. */
  repository?: { full_name?: string; html_url?: string; owner?: GitHubUser; [field: string]: unknown };
  /** Snippets, only when the Accept header asked for `text-match`; normally absent. */
  text_matches?: { fragment?: string; matches?: unknown[]; [field: string]: unknown }[];
  [field: string]: unknown;
}

/** GitHub's code search envelope; `items` is empty when nothing matched. */
export interface SearchCodeList {
  total_count?: number;
  incomplete_results?: boolean;
  items?: SearchCodeItem[];
  [field: string]: unknown;
}

/** One commit hit from `/search/commits`. */
export interface SearchCommitItem {
  sha: string;
  html_url?: string;
  url?: string;
  comments_url?: string;
  score?: number;
  commit?: {
    author?: { name?: string; email?: string; date?: string; [field: string]: unknown } | null;
    committer?: { name?: string; email?: string; date?: string; [field: string]: unknown } | null;
    message?: string;
    comment_count?: number;
    [field: string]: unknown;
  };
  author?: GitHubUser | null;
  committer?: GitHubUser | null;
  parents?: { sha?: string; url?: string; html_url?: string; [field: string]: unknown }[];
  repository?: { full_name?: string; html_url?: string; owner?: GitHubUser; [field: string]: unknown };
  [field: string]: unknown;
}

/** GitHub's commit search envelope; `items` is empty when nothing matched. */
export interface SearchCommitList {
  total_count?: number;
  incomplete_results?: boolean;
  items?: SearchCommitItem[];
  [field: string]: unknown;
}

/** A repository, as `/repos/{owner}/{name}` returns it. */
export interface Repo {
  id: number;
  name?: string;
  full_name?: string;
  owner?: GitHubUser;
  private?: boolean;
  fork?: boolean;
  archived?: boolean;
  disabled?: boolean;
  description?: string | null;
  homepage?: string | null;
  html_url?: string;
  url?: string;
  default_branch?: string;
  language?: string | null;
  topics?: string[];
  visibility?: string;
  size?: number;
  stargazers_count?: number;
  watchers_count?: number;
  forks_count?: number;
  open_issues_count?: number;
  subscribers_count?: number;
  license?: { key?: string; name?: string; spdx_id?: string | null; [field: string]: unknown } | null;
  created_at?: string;
  updated_at?: string;
  pushed_at?: string;
  [field: string]: unknown;
}

/** One entry of a directory listing (`type` is `file`, `dir`, `symlink` or `submodule`). */
export interface ContentEntry {
  type?: string;
  name?: string;
  path?: string;
  sha?: string;
  /** Size in bytes; 0 for directories. */
  size?: number;
  html_url?: string;
  url?: string;
  git_url?: string | null;
  download_url?: string | null;
  [field: string]: unknown;
}

/** A file from `contents/{path}`, with GitHub's base64 `content` replaced by the decoded text. */
export interface ContentFile {
  type?: string;
  name?: string;
  path?: string;
  sha?: string;
  /** The file's real size in bytes, even when `content` was cut. */
  size?: number;
  /** Decoded UTF-8 text, cut to `max_bytes`; `encoding` is always `"utf-8"` afterwards. */
  content?: string;
  encoding?: string;
  /** True when the file is larger than `max_bytes`, so `content` holds only the first part. */
  truncated?: boolean;
  html_url?: string;
  url?: string;
  git_url?: string | null;
  download_url?: string | null;
  [field: string]: unknown;
}

/** A directory listing from `contents/{path}`. */
export interface ContentDirectory {
  entries: ContentEntry[];
}

/** One node of a git tree. */
export interface TreeEntry {
  path?: string;
  /** `blob`, `tree` or `commit` (a submodule). */
  mode?: string;
  type?: string;
  sha?: string;
  /** Size in bytes for blobs; absent for trees. */
  size?: number;
  url?: string;
  [field: string]: unknown;
}

/** A git tree, as returned by `git/trees/{ref}`. */
export interface TreeResponse {
  sha?: string;
  url?: string;
  tree?: TreeEntry[];
  /** True when the tree was too large for GitHub to return in full; use `get_file` for the rest. */
  truncated?: boolean;
  [field: string]: unknown;
}

/** A branch, as `/branches` returns it. */
export interface Branch {
  name?: string;
  protected?: boolean;
  commit?: { sha?: string; url?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** A changed file inside a commit or comparison. */
export interface CommitFile {
  filename?: string;
  previous_filename?: string;
  status?: string;
  additions?: number;
  deletions?: number;
  changes?: number;
  /** Unified diff hunk; absent for binary files and large files. */
  patch?: string;
  sha?: string;
  blob_url?: string;
  raw_url?: string;
  contents_url?: string;
  [field: string]: unknown;
}

/** A commit, as `/commits/{ref}` returns it (also the shape of comparison entries). */
export interface CommitDetail {
  sha?: string;
  html_url?: string;
  url?: string;
  comments_url?: string;
  commit?: {
    author?: { name?: string; email?: string; date?: string; [field: string]: unknown } | null;
    committer?: { name?: string; email?: string; date?: string; [field: string]: unknown } | null;
    message?: string;
    comment_count?: number;
    tree?: { sha?: string; url?: string; [field: string]: unknown };
    verification?: { verified?: boolean; reason?: string; [field: string]: unknown };
    [field: string]: unknown;
  };
  author?: GitHubUser | null;
  committer?: GitHubUser | null;
  parents?: { sha?: string; url?: string; html_url?: string; [field: string]: unknown }[];
  stats?: { additions?: number; deletions?: number; total?: number; [field: string]: unknown };
  /** Changed files with their patches; omitted by GitHub on very large commits. */
  files?: CommitFile[];
  [field: string]: unknown;
}

/** A comparison of two commits, as `compare/{base}...{head}` returns it. */
export interface CompareResult {
  status?: string;
  ahead_by?: number;
  behind_by?: number;
  total_commits?: number;
  commits?: CommitDetail[];
  files?: CommitFile[];
  base_commit?: CommitDetail;
  merge_base_commit?: CommitDetail;
  html_url?: string;
  url?: string;
  permalink_url?: string;
  diff_url?: string;
  patch_url?: string;
  [field: string]: unknown;
}

/**
 * Search issues and pull requests across GitHub, like `gh search issues` / `gh search prs`.
 * `query` is GitHub's search syntax (`repo:owner/name is:open label:bug`, `author:me`, free
 * text, ...); `type` appends `is:pr` or `is:issue` so one call covers only PRs or only issues
 * (omit it for both). `sort` is `comments`, `reactions`, `created`, `updated` or `interactions`
 * (default best match); `order` is `asc` or `desc`. Paginate with `per_page` (default 30, max
 * 100) and `page`. Returns GitHub's `{ total_count, incomplete_results, items }` unchanged, with
 * each item's repository API URL in `repository_url`.
 */
export async function search_issues(
  args: {
    query: string;
    type?: "pr" | "issue";
    sort?: string;
    order?: "asc" | "desc";
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<SearchIssueList> {
  const type = args.type;
  if (type !== undefined && type !== "pr" && type !== "issue") {
    throw new Error('type must be "pr" or "issue"');
  }
  const text = required(args.query, "query");
  const q = type ? `${text} is:${type}` : text;
  return gh(ctx, "GET", "/search/issues", {
    params: {
      q,
      sort: args.sort,
      order: args.order,
      per_page: args.per_page,
      page: args.page,
    },
  });
}

/**
 * Search file contents across GitHub, like `gh search code`. `query` is GitHub's code search
 * syntax (`repo:owner/name`, `path:src`, `language:ts`, `filename:foo`, free text); code search
 * needs at least one search term and only indexes a repository's default branch. There is no
 * sort order. Paginate with `per_page` (default 30, max 100) and `page`; results are capped at
 * 1000 by GitHub. Returns GitHub's `{ total_count, incomplete_results, items }` unchanged; each
 * item carries `path`, `sha` and the embedded `repository`.
 */
export async function search_code(
  args: { query: string; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<SearchCodeList> {
  return gh(ctx, "GET", "/search/code", {
    params: {
      q: required(args.query, "query"),
      per_page: args.per_page,
      page: args.page,
    },
  });
}

/**
 * Search commit messages across GitHub, like `gh search commits`. `query` is GitHub's commit
 * search syntax (`repo:owner/name`, `author:login`, `committer:login`, `author-date:>2024-01-01`,
 * `merge:false`, free text); commit search needs at least one search term. `sort` is
 * `author-date` or `committer-date` (default best match); `order` is `asc` or `desc`. Paginate
 * with `per_page` (default 30, max 100) and `page`. Returns GitHub's
 * `{ total_count, incomplete_results, items }` unchanged.
 */
export async function search_commits(
  args: {
    query: string;
    sort?: "author-date" | "committer-date";
    order?: "asc" | "desc";
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<SearchCommitList> {
  return gh(ctx, "GET", "/search/commits", {
    params: {
      q: required(args.query, "query"),
      sort: args.sort,
      order: args.order,
      per_page: args.per_page,
      page: args.page,
    },
  });
}

/**
 * One repository's metadata, as `{ repo }`: description, `default_branch`, `private`, `archived`,
 * `visibility`, language, topics, star/watch/fork/open-issue counts, license and timestamps.
 * `repo` is `"owner/name"`.
 */
export async function get_repo(args: { repo: string }, ctx: Ctx): Promise<{ repo: Repo }> {
  return { repo: await gh(ctx, "GET", repoPath(args.repo)) };
}

/**
 * Read a file or list a directory at `path` in `repo` (`"owner/name"`), at `ref` (branch, tag or
 * commit SHA; default the repository's default branch). For a directory the result is
 * `{ entries }`, each with `type` (`file`, `dir`, `symlink`, `submodule`), `path`, `sha` and
 * `size`. For a file it is GitHub's file metadata with `content` replaced by the decoded UTF-8
 * text and `encoding` set to `"utf-8"`, cut to `max_bytes` (default 200000) with `truncated`
 * true when the file is larger (`size` still reports the real size); binary files decode to
 * replacement characters. Files GitHub will not inline (over 1 MB) are fetched raw and bounded
 * the same way.
 */
export async function get_file(
  args: { repo: string; path: string; ref?: string; max_bytes?: number },
  ctx: Ctx,
): Promise<ContentFile | ContentDirectory> {
  const repo = repoPath(args.repo);
  const path = filePath(args.path);
  if (!path) throw new Error("path is required");
  const limit = args.max_bytes ?? 200000;
  const params = { ref: args.ref };
  const payload = await gh(ctx, "GET", `${repo}/contents/${path}`, { params });
  if (Array.isArray(payload)) return { entries: payload as ContentEntry[] };
  const encoded = typeof payload.content === "string" ? payload.content.replace(/\s+/g, "") : "";
  // Files over 1 MB come without inline content (`encoding: "none"`); fetch them raw instead.
  const text: string = encoded
    ? ctx.base64.decodeText(encoded)
    : await gh(ctx, "GET", `${repo}/contents/${path}`, {
        params,
        accept: "application/vnd.github.raw+json",
        text: true,
      });
  const clipped = clip(text, limit, false);
  return { ...payload, content: clipped.text, encoding: "utf-8", truncated: clipped.truncated };
}

/**
 * List a git tree, like `gh api git/trees/{ref}`: the repository's files and directories at `ref`
 * (branch, tag or commit SHA; default `HEAD`, the default branch). Pass `recursive: true` to
 * flatten the whole tree into one list of `path`s. Returns GitHub's `{ sha, url, tree, truncated }`
 * unchanged; `truncated` means the tree exceeded GitHub's limit and the listing is incomplete
 * (use `get_file` for the missing subtrees). A recursive tree of a very large repository can
 * exceed the 10 MiB response limit; list a subdirectory with `ref: "<branch>:<dir>"` instead.
 * Each entry has `path`, `mode`, `type` (`blob`, `tree` or `commit`), `sha` and, for blobs,
 * `size`.
 */
export async function get_tree(
  args: { repo: string; ref?: string; recursive?: boolean },
  ctx: Ctx,
): Promise<TreeResponse> {
  const ref = (args.ref || "").trim() || "HEAD";
  return gh(ctx, "GET", `${repoPath(args.repo)}/git/trees/${encodeURIComponent(ref)}`, {
    params: { recursive: args.recursive ? 1 : undefined },
  });
}

/**
 * List a repository's branches as `{ branches }`, each with `name`, `protected` and its head
 * `commit` `sha`. `protected: true` returns only protected branches (available on some plans).
 * Paginate with `per_page` (default 30, max 100) and `page`.
 */
export async function list_branches(
  args: { repo: string; protected?: boolean; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<{ branches: Branch[] }> {
  const branches = await gh(ctx, "GET", `${repoPath(args.repo)}/branches`, {
    params: {
      protected: args.protected,
      per_page: args.per_page,
      page: args.page,
    },
  });
  return { branches };
}

/**
 * One commit as `{ commit }`: `sha`, `commit.message`, `commit.author`/`commit.committer` (name,
 * email, date), the verified `commit.verification`, the linked GitHub `author`/`committer`,
 * `parents`, `stats` and `files` with their diffs in `patch` (absent for binary and very large
 * files). `ref` is a commit SHA, branch or tag. `repo` is `"owner/name"`.
 */
export async function get_commit(
  args: { repo: string; ref: string },
  ctx: Ctx,
): Promise<{ commit: CommitDetail }> {
  const ref = (args.ref || "").trim();
  if (!ref) throw new Error("ref is required");
  const commit = await gh(ctx, "GET", `${repoPath(args.repo)}/commits/${encodeURIComponent(ref)}`);
  return { commit };
}

/**
 * Compare two commits, branches or tags, like `gh api compare/{base}...{head}`: returns GitHub's
 * comparison object unchanged — `status` (`ahead`, `behind`, `diverged` or `identical`),
 * `ahead_by`, `behind_by`, `total_commits`, the `commits` between them, and `files` with unified
 * `patch` hunks. `base` and `head` are SHAs, branches or tags. Results can be large: GitHub returns
 * at most 250 commits and 300 files; paginate with `per_page` (default 30, max 100) and `page`;
 * for a pure diff use `request` with `accept: "application/vnd.github.diff"`.
 */
export async function compare_commits(
  args: { repo: string; base: string; head: string; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<CompareResult> {
  const base = (args.base || "").trim();
  const head = (args.head || "").trim();
  if (!base || !head) throw new Error("base and head are required");
  return gh(
    ctx,
    "GET",
    `${repoPath(args.repo)}/compare/${encodeURIComponent(base)}...${encodeURIComponent(head)}`,
    { params: { per_page: args.per_page, page: args.page } },
  );
}

// ---- pulls ----

// Pull requests: listing, inspecting, creating, updating, merging, diffs, files, checks and
// reviews. Everything below is the pull-request side of `gh pr ...` that does not need a local
// checkout; the `request` escape hatch above reaches anything not wrapped here (closing issues,
// labels on issues, ...).

/** One side (`head` or `base`) of a pull request: branch name, commit and repository. */
export interface PullRef {
  ref?: string;
  sha?: string;
  label?: string;
  repo?: {
    full_name?: string;
    name?: string;
    owner?: GitHubUser;
    [field: string]: unknown;
  } | null;
  [field: string]: unknown;
}

/** A pull request, as GitHub sends it (`GET /repos/{owner}/{repo}/pulls/{number}`). */
export interface PullRequest {
  id: number;
  number: number;
  state?: string;
  title?: string;
  body?: string | null;
  draft?: boolean;
  merged?: boolean;
  mergeable?: boolean | null;
  mergeable_state?: string;
  user?: GitHubUser;
  labels?: GitHubLabel[];
  head?: PullRef;
  base?: PullRef;
  html_url?: string;
  url?: string;
  created_at?: string;
  updated_at?: string;
  closed_at?: string | null;
  merged_at?: string | null;
  additions?: number;
  deletions?: number;
  changed_files?: number;
  commits?: number;
  comments?: number;
  review_comments?: number;
  requested_reviewers?: GitHubUser[];
  [field: string]: unknown;
}

/** One changed file of a pull request (`GET .../pulls/{number}/files`). */
export interface PullFile {
  filename: string;
  status?: string;
  sha?: string;
  additions?: number;
  deletions?: number;
  changes?: number;
  blob_url?: string;
  raw_url?: string;
  contents_url?: string;
  previous_filename?: string;
  /** Unified-diff hunk, absent for binary or very large files. */
  patch?: string;
  [field: string]: unknown;
}

/** The result of a merge attempt (`PUT .../pulls/{number}/merge`). */
export interface PullMergeResult {
  merged?: boolean;
  sha?: string;
  message?: string;
  [field: string]: unknown;
}

/** One check run on a commit (`GET /repos/{owner}/{repo}/commits/{ref}/check-runs`). */
export interface CheckRun {
  id: number;
  name?: string;
  head_sha?: string;
  /** `queued`, `in_progress`, `completed`, `waiting`, `requested`, `pending`. */
  status?: string;
  /** `success`, `failure`, `neutral`, `cancelled`, `skipped`, `timed_out`, `action_required`; null until completed. */
  conclusion?: string | null;
  started_at?: string | null;
  completed_at?: string | null;
  details_url?: string;
  html_url?: string;
  external_id?: string | null;
  output?: {
    title?: string | null;
    summary?: string | null;
    text?: string | null;
    annotations_count?: number;
    [field: string]: unknown;
  };
  app?: {
    slug?: string;
    name?: string;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** One legacy commit status inside a combined status (`GET .../commits/{ref}/status`). */
export interface CombinedStatusEntry {
  id?: number;
  state?: string;
  context?: string;
  description?: string | null;
  target_url?: string | null;
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

/** The combined commit status: `state` is `success`, `pending`, `failure` or `error`. */
export interface CombinedStatus {
  state: string;
  sha?: string;
  total_count?: number;
  statuses?: CombinedStatusEntry[];
  [field: string]: unknown;
}

/** A pull request review (`GET .../pulls/{number}/reviews`). */
export interface Review {
  id: number;
  user?: GitHubUser;
  /** `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `PENDING` or `DISMISSED`. */
  state?: string;
  body?: string | null;
  commit_id?: string;
  html_url?: string;
  submitted_at?: string | null;
  [field: string]: unknown;
}

/** An inline review comment on a pull request (`GET .../pulls/{number}/comments`). */
export interface ReviewComment {
  id: number;
  path?: string;
  body?: string;
  line?: number | null;
  original_line?: number | null;
  side?: string;
  start_line?: number | null;
  diff_hunk?: string;
  user?: GitHubUser;
  commit_id?: string;
  created_at?: string;
  updated_at?: string;
  html_url?: string;
  in_reply_to_id?: number;
  [field: string]: unknown;
}

/** One inline comment in a review being submitted: a file `path`, a `line` in the diff, and its `body`. */
export interface ReviewCommentInput {
  path: string;
  /** Line in the file (or in the diff when `side` is `LEFT`/`RIGHT`) the comment attaches to. */
  line: number;
  /** `RIGHT` (default) comments on the new version, `LEFT` on the old one. */
  side?: "LEFT" | "RIGHT";
  /** First line of a multi-line comment; pair with `start_side`. */
  start_line?: number;
  start_side?: "LEFT" | "RIGHT";
  body: string;
}

/** `/repos/{owner}/{repo}/pulls/{number}`, validating the pull request number. */
function pullPath(repo: string, number: number): string {
  return `${repoPath(repo)}/pulls/${positive(number, "number")}`;
}

/**
 * List a repository's pull requests as `{ pull_requests }` (each a full pull request object).
 * `state` is `open` (default), `closed`, or `all`. `head` filters by `"user:branch"` and `base` by
 * branch name. `sort` is `created` (default), `updated`, `popularity` or `long_wait_time`, ordered
 * by `direction` (`desc` default, or `asc`). Page with `per_page` (default 30, max 100) and `page`.
 * For filtered searches use `search_issues`; this endpoint ignores most other qualifiers.
 */
export async function list_prs(
  args: {
    repo: string;
    state?: "open" | "closed" | "all";
    head?: string;
    base?: string;
    sort?: "created" | "updated" | "popularity" | "long_wait_time";
    direction?: "asc" | "desc";
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<{ pull_requests: PullRequest[] }> {
  const pull_requests = await gh(ctx, "GET", `${repoPath(args.repo)}/pulls`, {
    params: {
      state: args.state,
      head: args.head,
      base: args.base,
      sort: args.sort,
      direction: args.direction,
      per_page: args.per_page,
      page: args.page,
    },
  });
  return { pull_requests };
}

/**
 * One pull request by number, as `{ pull_request }`: title, body, state, `draft`, `merged`,
 * `mergeable`/`mergeable_state`, branches (`head`/`base` refs and shas), diff stats and labels.
 * `mergeable` is null while GitHub is still computing it; call again to get the answer.
 */
export async function get_pr(
  args: { repo: string; number: number },
  ctx: Ctx,
): Promise<{ pull_request: PullRequest }> {
  return { pull_request: await gh(ctx, "GET", pullPath(args.repo, args.number)) };
}

/**
 * Open a pull request as `{ pull_request }`. `head` is the branch with the commits: `"branch"` for
 * a branch in this repository, `"owner:branch"` for a fork. The head branch must already be
 * pushed; this does not create, commit or upload anything, and a missing branch fails with 422.
 * `base` is the branch to merge into (such as `main`). `draft: true` opens it as a draft.
 * `maintainer_can_modify` (default true) lets maintainers of the base repository push to a fork's
 * head branch. `body` is Markdown; you may pass `Closes #123` to link issues.
 */
export async function create_pr(
  args: {
    repo: string;
    title: string;
    head: string;
    base: string;
    body?: string;
    draft?: boolean;
    maintainer_can_modify?: boolean;
  },
  ctx: Ctx,
): Promise<{ pull_request: PullRequest }> {
  if (!args.title || !args.title.trim()) throw new Error("title is required");
  if (!args.head || !args.head.trim()) throw new Error("head is required (the already-pushed branch)");
  if (!args.base || !args.base.trim()) throw new Error("base is required");
  const body = defined({
    title: args.title,
    head: args.head.trim(),
    base: args.base.trim(),
    body: args.body,
    draft: args.draft,
    maintainer_can_modify: args.maintainer_can_modify,
  });
  return { pull_request: await gh(ctx, "POST", `${repoPath(args.repo)}/pulls`, { body }) };
}

/**
 * Update a pull request's `title`, `body`, `state` (`open` or `closed`) or `base` branch; only the
 * fields you pass are sent. At least one is required. Closing with `state: "closed"` is how a pull
 * request is abandoned. Labels and assignees are issue fields: use `update_issue` with the PR number.
 * Draft state is GraphQL-only: `request` with `POST /graphql` and `markPullRequestReadyForReview`.
 * Returns `{ pull_request }`.
 */
export async function update_pr(
  args: {
    repo: string;
    number: number;
    title?: string;
    body?: string;
    state?: "open" | "closed";
    base?: string;
  },
  ctx: Ctx,
): Promise<{ pull_request: PullRequest }> {
  const body = defined({
    title: args.title,
    body: args.body,
    state: args.state,
    base: args.base,
  });
  if (Object.keys(body).length === 0) {
    throw new Error("nothing to update: pass at least one of title, body, state, base");
  }
  return { pull_request: await gh(ctx, "PATCH", pullPath(args.repo, args.number), { body }) };
}

/**
 * Merge a pull request, returning GitHub's own `{ merged, sha, message }` — `merged: false` (with a
 * `message` such as "Pull Request is not mergeable") means the merge did not happen. `merge_method`
 * is `merge` (default, a merge commit), `squash` or `rebase`. `commit_title` and `commit_message`
 * set the merge/squash commit text; `sha` makes the merge conditional on the head still being that
 * commit, so a concurrent push fails it with 409. GitHub answers 405 when the pull request is not
 * mergeable (conflicts, required checks or reviews missing); check `get_pr` first.
 */
export async function merge_pr(
  args: {
    repo: string;
    number: number;
    merge_method?: "merge" | "squash" | "rebase";
    commit_title?: string;
    commit_message?: string;
    sha?: string;
  },
  ctx: Ctx,
): Promise<PullMergeResult> {
  const body = defined({
    merge_method: args.merge_method,
    commit_title: args.commit_title,
    commit_message: args.commit_message,
    sha: args.sha,
  });
  return gh(ctx, "PUT", `${pullPath(args.repo, args.number)}/merge`, { body });
}

/**
 * A pull request's unified diff as `{ diff, bytes, truncated }`. `max_bytes` (default 100000) caps
 * the returned text; `bytes` is the UTF-8 length actually returned and `truncated` says whether the
 * diff was longer, so fetch a single file's `patch` from `list_pr_files` when it was. Accurate but
 * expensive for very large pull requests; `list_pr_files` gives per-file patches without the cap.
 */
export async function get_pr_diff(
  args: { repo: string; number: number; max_bytes?: number },
  ctx: Ctx,
): Promise<{ diff: string; bytes: number; truncated: boolean }> {
  const requested = Number(args.max_bytes ?? 100000);
  const maxBytes = Number.isFinite(requested) && requested > 0 ? Math.trunc(requested) : 100000;
  const text = await gh(ctx, "GET", pullPath(args.repo, args.number), {
    accept: "application/vnd.github.diff",
    text: true,
  });
  const clipped = clip(text, maxBytes, false);
  return { diff: clipped.text, bytes: clipped.bytes, truncated: clipped.truncated };
}

/**
 * The files changed by a pull request as `{ files }`: `filename`, `status` (`added`, `modified`,
 * `removed`, `renamed`, ...), `additions`, `deletions`, `changes` and the unified-diff `patch`
 * (absent for binary or very large files). Page with `per_page` (default 30, max 100) and `page`;
 * GitHub caps this endpoint at 3000 files.
 */
export async function list_pr_files(
  args: { repo: string; number: number; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<{ files: PullFile[] }> {
  const files = await gh(ctx, "GET", `${pullPath(args.repo, args.number)}/files`, {
    params: { per_page: args.per_page, page: args.page },
  });
  return { files };
}

/**
 * CI state of a pull request's latest commit: resolves the head SHA, then reads that commit's check
 * runs and combined legacy commit status. Returns `{ head_sha, total_count, check_runs, state,
 * statuses }`: `check_runs` are GitHub's check-run objects (`name`, `status`, `conclusion`,
 * `details_url`, `output`, `app`), `total_count` counts them, and `state`/`statuses` come from the
 * combined status (`success`, `pending`, `failure` or `error`, plus the older commit statuses).
 * Nothing is combined for you: checks are pending while any run is not `completed`, and failing
 * when a `conclusion` is `failure`, `timed_out` or `cancelled`. `state` is null when no legacy
 * statuses exist. Page the check runs with `per_page` (default 30, max 100) and `page`. For a fork
 * pull request the checks live on the fork, so that repository is queried.
 */
export async function get_pr_checks(
  args: { repo: string; number: number; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<{
  head_sha: string;
  total_count: number;
  check_runs: CheckRun[];
  state: string | null;
  statuses: CombinedStatusEntry[];
}> {
  const pull = await gh(ctx, "GET", pullPath(args.repo, args.number));
  const headSha: string = pull?.head?.sha ?? "";
  if (!headSha) throw new Error("pull request has no head sha yet");
  const headRepo: string = pull?.head?.repo?.full_name || args.repo;
  const repo = repoPath(headRepo);
  const params = { per_page: args.per_page, page: args.page };
  const runs = await gh(ctx, "GET", `${repo}/commits/${encodeURIComponent(headSha)}/check-runs`, { params });
  const combined: CombinedStatus = await gh(
    ctx,
    "GET",
    `${repo}/commits/${encodeURIComponent(headSha)}/status`,
    { params },
  );
  return {
    head_sha: headSha,
    total_count: runs?.total_count ?? 0,
    check_runs: runs?.check_runs ?? [],
    state: combined?.state ?? null,
    statuses: combined?.statuses ?? [],
  };
}

/**
 * A pull request's reviews as `{ reviews }`, oldest first: each has `user`, `state` (`APPROVED`,
 * `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED`, `PENDING`), `body`, `commit_id` and `submitted_at`.
 * One reviewer can appear several times (a dismissal plus a later approval); the last state per
 * user is what counts. Page with `per_page` (default 30, max 100) and `page`.
 */
export async function list_pr_reviews(
  args: { repo: string; number: number; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<{ reviews: Review[] }> {
  const reviews = await gh(ctx, "GET", `${pullPath(args.repo, args.number)}/reviews`, {
    params: { per_page: args.per_page, page: args.page },
  });
  return { reviews };
}

/**
 * A pull request's inline review comments as `{ review_comments }`, oldest first: `path`, `line`,
 * `side`, `body`, `diff_hunk`, `commit_id`, `user` and `in_reply_to_id` for replies. Includes
 * comments from every review, resolved or not. Page with `per_page` (default 30, max 100) and
 * `page`. For conversation (non-inline) comments use `list_comments`.
 */
export async function list_pr_review_comments(
  args: { repo: string; number: number; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<{ review_comments: ReviewComment[] }> {
  const review_comments = await gh(ctx, "GET", `${pullPath(args.repo, args.number)}/comments`, {
    params: { per_page: args.per_page, page: args.page },
  });
  return { review_comments };
}

/**
 * Submit a review as `{ review }`. `event` is `APPROVE`, `REQUEST_CHANGES` (both need a `body` when
 * `REQUEST_CHANGES`) or `COMMENT`; `body` is the Markdown summary. `comments` attaches inline
 * comments to the diff, each with the file `path`, the `line` in the new version (or the old one
 * with `side: "LEFT"`), an optional `start_line` for a multi-line range, and its own `body`. A line
 * GitHub cannot place fails the whole review with 422, so keep anchors inside the diff hunks.
 * Returns the created review; its `state` and `html_url` identify it.
 */
export async function create_pr_review(
  args: {
    repo: string;
    number: number;
    event: "APPROVE" | "REQUEST_CHANGES" | "COMMENT";
    body?: string;
    comments?: ReviewCommentInput[];
  },
  ctx: Ctx,
): Promise<{ review: Review }> {
  const event = args.event;
  if (event !== "APPROVE" && event !== "REQUEST_CHANGES" && event !== "COMMENT") {
    throw new Error('event must be "APPROVE", "REQUEST_CHANGES" or "COMMENT"');
  }
  if (event === "REQUEST_CHANGES" && !(args.body && args.body.trim())) {
    throw new Error("body is required when event is REQUEST_CHANGES");
  }
  const body = defined({
    event,
    body: args.body,
    comments: args.comments?.map((comment) => defined({ ...comment })),
  });
  return { review: await gh(ctx, "POST", `${pullPath(args.repo, args.number)}/reviews`, { body }) };
}

/**
 * Ask for reviews from users and/or teams, as `{ pull_request }` (the updated pull request).
 * `reviewers` are GitHub logins; `team_reviewers` are team slugs (no organization prefix) and need
 * the team to have access to the repository. At least one of the two is required. Someone who
 * already reviewed is not re-requested; a user cannot request themselves.
 */
export async function request_reviewers(
  args: { repo: string; number: number; reviewers?: string[]; team_reviewers?: string[] },
  ctx: Ctx,
): Promise<{ pull_request: PullRequest }> {
  const reviewers = (args.reviewers ?? []).filter((login) => Boolean(login && login.trim()));
  const teamReviewers = (args.team_reviewers ?? []).filter((team) => Boolean(team && team.trim()));
  if (reviewers.length === 0 && teamReviewers.length === 0) {
    throw new Error("pass at least one of reviewers or team_reviewers");
  }
  const body = defined({ reviewers, team_reviewers: teamReviewers });
  return {
    pull_request: await gh(ctx, "POST", `${pullPath(args.repo, args.number)}/requested_reviewers`, { body }),
  };
}

// ---- actions_issues ----

/** A GitHub Actions workflow run. */
export interface Run {
  id?: number;
  name?: string | null;
  display_title?: string;
  event?: string;
  status?: string | null;
  conclusion?: string | null;
  head_branch?: string | null;
  head_sha?: string;
  run_number?: number;
  run_attempt?: number;
  workflow_id?: number;
  html_url?: string;
  created_at?: string;
  updated_at?: string;
  run_started_at?: string;
  actor?: GitHubUser | null;
  [field: string]: unknown;
}

/** A page of workflow runs, as GitHub returns it (`{ total_count, workflow_runs }`). */
export interface RunList {
  total_count?: number;
  workflow_runs?: Run[];
  [field: string]: unknown;
}

/** One step of a workflow job. */
export interface JobStep {
  number?: number;
  name?: string;
  status?: string;
  conclusion?: string | null;
  started_at?: string | null;
  completed_at?: string | null;
  [field: string]: unknown;
}

/** A job within a workflow run. */
export interface Job {
  id?: number;
  run_id?: number;
  name?: string;
  status?: string;
  conclusion?: string | null;
  started_at?: string | null;
  completed_at?: string | null;
  html_url?: string;
  runner_name?: string | null;
  steps?: JobStep[];
  [field: string]: unknown;
}

/** A page of jobs, as GitHub returns it (`{ total_count, jobs }`). */
export interface JobList {
  total_count?: number;
  jobs?: Job[];
  [field: string]: unknown;
}

/** A bounded slice of a job log. */
export interface JobLog {
  /** The returned log text, by default the end of the log. */
  log: string;
  /** Size of `log` in bytes. */
  bytes: number;
  /** True when the log was cut; true for the head slice means the rest is after it. */
  truncated: boolean;
  [field: string]: unknown;
}

/** The result of asking GitHub to rerun or cancel a run. */
export interface RunAction {
  run_id: number;
  /** What was requested: `rerun`, `rerun-failed-jobs` or `cancel`. */
  action: string;
  [field: string]: unknown;
}

/** A workflow definition. */
export interface Workflow {
  id?: number;
  name?: string;
  /** Repository path of the workflow file, for example `.github/workflows/ci.yml`. */
  path?: string;
  /** `active`, `disabled_manually`, `disabled_inactivity` or `deleted`. */
  state?: string;
  html_url?: string;
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

/** A page of workflow definitions (`{ total_count, workflows }`). */
export interface WorkflowList {
  total_count?: number;
  workflows?: Workflow[];
  [field: string]: unknown;
}

/** The result of dispatching a workflow. */
export interface WorkflowDispatch {
  dispatched: boolean;
  /** The workflow id or file name that was dispatched, as given. */
  workflow: string;
  /** The git ref the run was started from. */
  ref: string;
  [field: string]: unknown;
}

/** A file attached to a release. Its bytes are not downloaded by this connector. */
export interface ReleaseAsset {
  id?: number;
  name?: string;
  size?: number;
  content_type?: string;
  browser_download_url?: string;
  created_at?: string;
  [field: string]: unknown;
}

/** A GitHub release. */
export interface Release {
  id?: number;
  tag_name?: string;
  name?: string | null;
  body?: string | null;
  draft?: boolean;
  prerelease?: boolean;
  target_commitish?: string;
  html_url?: string;
  created_at?: string;
  published_at?: string | null;
  author?: GitHubUser | null;
  assets?: ReleaseAsset[];
  [field: string]: unknown;
}

/** A page of releases; GitHub sorts them newest first. */
export interface ReleaseList {
  releases: Release[];
  [field: string]: unknown;
}

/** An issue. The same shape is used for pull requests, which `list_issues` also returns. */
export interface Issue {
  id?: number;
  number: number;
  title?: string;
  body?: string | null;
  /** `open` or `closed`. */
  state?: string;
  /** `completed`, `not_planned`, `reopened` or null. */
  state_reason?: string | null;
  html_url?: string;
  user?: GitHubUser | null;
  labels?: (GitHubLabel | string)[];
  assignees?: GitHubUser[] | null;
  /** Number of comments on the issue or pull request. */
  comments?: number;
  created_at?: string;
  updated_at?: string;
  closed_at?: string | null;
  closed_by?: GitHubUser | null;
  /** Present when this "issue" is actually a pull request. */
  pull_request?: Record<string, unknown>;
  [field: string]: unknown;
}

/** A page of issues; GitHub's list endpoint includes pull requests. */
export interface IssueList {
  issues: Issue[];
  [field: string]: unknown;
}

/** A comment on an issue or pull request conversation. */
export interface Comment {
  id?: number;
  body?: string;
  user?: GitHubUser | null;
  html_url?: string;
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

/** A page of comments, oldest first. */
export interface CommentList {
  comments: Comment[];
  [field: string]: unknown;
}

/** A workflow id or file name as a path segment (`12345` or `ci.yml`). */
function workflowSegment(value: string | number): string {
  const text = String(value ?? "").trim();
  if (!text) throw new Error("workflow must be a workflow id or file name such as ci.yml");
  return encodeURIComponent(text);
}

/** GitHub takes labels as one comma-separated parameter, not repeated ones. */
function labelList(labels: string | string[] | undefined): string | undefined {
  if (labels === undefined) return undefined;
  const list = Array.isArray(labels) ? labels : labels.split(",");
  const cleaned = list.map((label) => label.trim()).filter(Boolean);
  return cleaned.length ? cleaned.join(",") : undefined;
}

/**
 * List workflow runs for a repository, newest first. Returns GitHub's
 * `{ total_count, workflow_runs }` unchanged; pass `workflow_runs[].id` to `get_run`,
 * `list_run_jobs` or `rerun_run`. One page at a time: `per_page` defaults to 30 (max 100),
 * `page` starts at 1.
 *
 * - `workflow`: restrict to one workflow, by numeric id or file name (`ci.yml`); lists
 *   every workflow when omitted.
 * - `branch`: branch the run was started on.
 * - `event`: trigger, such as `push`, `pull_request`, `schedule`, `workflow_dispatch`
 *   or `release`.
 * - `status`: `queued`, `in_progress`, `completed`, `requested`, `waiting`, `pending`,
 *   or a conclusion (`success`, `failure`, `cancelled`, `skipped`, `timed_out`,
 *   `action_required`, `neutral`, `stale`). `completed` alone then filter the returned
 *   `conclusion` for success/failure.
 * - `head_sha`: exact commit SHA the run was created for.
 * - `actor`: login of the user who triggered the run.
 */
export async function list_runs(
  args: {
    repo: string;
    workflow?: string | number;
    branch?: string;
    event?: string;
    status?: string;
    head_sha?: string;
    actor?: string;
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<RunList> {
  const root = repoPath(args.repo);
  const path =
    args.workflow === undefined || String(args.workflow).trim() === ""
      ? `${root}/actions/runs`
      : `${root}/actions/workflows/${workflowSegment(args.workflow)}/runs`;
  return gh(ctx, "GET", path, {
    params: {
      branch: args.branch,
      event: args.event,
      status: args.status,
      head_sha: args.head_sha,
      actor: args.actor,
      per_page: args.per_page ?? 30,
      page: args.page,
    },
  });
}

/**
 * Read one workflow run by id, as `{ run }`. Use it to poll a run after `dispatch_workflow`
 * or `rerun_run`: `status` is `queued` / `in_progress` / `completed`, and only a completed
 * run has a final `conclusion`. `run_attempt` increments on each rerun.
 */
export async function get_run(args: { repo: string; run_id: number }, ctx: Ctx): Promise<{ run: Run }> {
  const path = `${repoPath(args.repo)}/actions/runs/${positive(args.run_id, "run_id")}`;
  return { run: await gh(ctx, "GET", path) };
}

/**
 * List the jobs of a run, as GitHub's `{ total_count, jobs }`. Each job carries its own
 * `status`, `conclusion`, timings and `steps`, so this is the way to find which job failed
 * before fetching its log with `get_job_log`. One page at a time: `per_page` defaults to 30
 * (max 100), `page` starts at 1.
 *
 * `filter` is `latest` (default, jobs from the most recent attempt of the run) or `all`
 * (every attempt, including reruns).
 */
export async function list_run_jobs(
  args: { repo: string; run_id: number; filter?: "latest" | "all"; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<JobList> {
  const path = `${repoPath(args.repo)}/actions/runs/${positive(args.run_id, "run_id")}/jobs`;
  return gh(ctx, "GET", path, {
    params: {
      filter: args.filter ?? "latest",
      per_page: args.per_page ?? 30,
      page: args.page,
    },
  });
}

/**
 * Fetch the plain-text log of one job, as `{ log, bytes, truncated }`. Job logs are huge, so
 * by default only the LAST `max_bytes` bytes (default 50000, about 12k tokens) are returned:
 * failures are at the end. Set `from_start: true` to keep the beginning instead, and raise
 * `max_bytes` to see more.
 *
 * `truncated: true` means the log was cut; `bytes` is the size of the returned `log` (a
 * `from_start` slice that is truncated keeps reading forward, a tail slice ends at the log
 * end). Logs are not available until the job has started; a job that is still queued, or a
 * log over the runtime's ~10 MiB response limit, raises an error. Step-level logs are not
 * exposed by the REST API: read the text and find the step markers.
 */
export async function get_job_log(
  args: { repo: string; job_id: number; max_bytes?: number; from_start?: boolean },
  ctx: Ctx,
): Promise<JobLog> {
  const path = `${repoPath(args.repo)}/actions/jobs/${positive(args.job_id, "job_id")}/logs`;
  const text: string = await gh(ctx, "GET", path, { text: true });
  const max_bytes = Math.max(1, Math.floor(args.max_bytes ?? 50000));
  const slice = clip(text, max_bytes, args.from_start !== true);
  return { log: slice.text, bytes: slice.bytes, truncated: slice.truncated };
}

/**
 * Rerun a completed workflow run. Returns `{ run_id, action }` once GitHub has accepted the
 * request (HTTP 202); poll `get_run` to watch the new `run_attempt`. With `failed_only: true`
 * only the failed jobs are rerun (GitHub's rerun-failed-jobs), otherwise the whole workflow
 * runs again with the original inputs. The run must be finished (not queued or in progress).
 */
export async function rerun_run(
  args: { repo: string; run_id: number; failed_only?: boolean },
  ctx: Ctx,
): Promise<RunAction> {
  const run_id = positive(args.run_id, "run_id");
  const failed_only = args.failed_only === true;
  const path = `${repoPath(args.repo)}/actions/runs/${run_id}/${failed_only ? "rerun-failed-jobs" : "rerun"}`;
  await gh(ctx, "POST", path);
  return { run_id: Number(run_id), action: failed_only ? "rerun-failed-jobs" : "rerun" };
}

/**
 * Cancel a workflow run that is queued or in progress. Returns `{ run_id, action: "cancel" }`
 * once GitHub has accepted the request; the run then moves to `completed` with conclusion
 * `cancelled`, which `get_run` confirms. Jobs that already finished keep their results.
 */
export async function cancel_run(args: { repo: string; run_id: number }, ctx: Ctx): Promise<RunAction> {
  const run_id = positive(args.run_id, "run_id");
  await gh(ctx, "POST", `${repoPath(args.repo)}/actions/runs/${run_id}/cancel`);
  return { run_id: Number(run_id), action: "cancel" };
}

/**
 * List the repository's workflow definitions, as GitHub's `{ total_count, workflows }`; each
 * entry's `path` (`.github/workflows/ci.yml`) or `id` can be passed to `list_runs`,
 * `dispatch_workflow`, `rerun_run` or `request`. One page at a time: `per_page` defaults to 30
 * (max 100), `page` starts at 1.
 */
export async function list_workflows(
  args: { repo: string; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<WorkflowList> {
  return gh(ctx, "GET", `${repoPath(args.repo)}/actions/workflows`, {
    params: { per_page: args.per_page ?? 30, page: args.page },
  });
}

/**
 * Start a `workflow_dispatch` run. Returns `{ dispatched, workflow, ref }` with HTTP 204
 * accepted; GitHub does not return the new run id, so poll `list_runs` with the same
 * `workflow` and `branch: ref` (newest first) for the run it created.
 *
 * - `workflow`: workflow id or file name, such as `deploy.yml`.
 * - `ref`: git ref (branch or tag) to run from, for example `main`; required.
 * - `inputs`: string values for the workflow's declared dispatch inputs. GitHub rejects
 *   undeclared inputs and missing required ones with a 422; booleans and numbers are passed
 *   as strings.
 *
 * The workflow file must already contain a `workflow_dispatch` trigger on that ref.
 */
export async function dispatch_workflow(
  args: { repo: string; workflow: string | number; ref: string; inputs?: Record<string, string> },
  ctx: Ctx,
): Promise<WorkflowDispatch> {
  const ref = (args.ref || "").trim();
  if (!ref) throw new Error("ref is required, for example main");
  const path = `${repoPath(args.repo)}/actions/workflows/${workflowSegment(args.workflow)}/dispatches`;
  await gh(ctx, "POST", path, {
    body: args.inputs === undefined ? { ref } : { ref, inputs: args.inputs },
  });
  return { dispatched: true, workflow: String(args.workflow), ref };
}

/**
 * List the repository's releases, newest published first, as `{ releases }`. Drafts and
 * prereleases are included. Use `get_release` for one tag, `list_releases` plus `assets` to
 * inspect attached files. One page at a time: `per_page` defaults to 30 (max 100), `page`
 * starts at 1.
 */
export async function list_releases(
  args: { repo: string; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<ReleaseList> {
  const releases: Release[] = await gh(ctx, "GET", `${repoPath(args.repo)}/releases`, {
    params: { per_page: args.per_page ?? 30, page: args.page },
  });
  return { releases };
}

/**
 * Read one release as `{ release }`. With `tag` it is the release for exactly that tag
 * (`v1.2.3`); without `tag` it is the repository's latest non-draft, non-prerelease release.
 * `body` is the release notes markdown, and `assets` lists attached files with their
 * `browser_download_url` (this connector does not download them).
 */
export async function get_release(args: { repo: string; tag?: string }, ctx: Ctx): Promise<{ release: Release }> {
  const root = `${repoPath(args.repo)}/releases`;
  const tag = (args.tag || "").trim();
  const path = tag ? `${root}/tags/${encodeURIComponent(tag)}` : `${root}/latest`;
  return { release: await gh(ctx, "GET", path) };
}

/**
 * List issues with GitHub's filters, as `{ issues }` (one page per call). Important: this
 * endpoint returns PULL REQUESTS TOO, and every returned object is an issue-shaped summary;
 * entries with a `pull_request` field are pull requests, so filter them out with
 * `issues.filter((issue) => !issue.pull_request)` for issues only.
 *
 * - `state`: `open` (default), `closed` or `all`.
 * - `labels`: label names, as an array or a comma-separated string; an issue must carry all
 *   of them.
 * - `assignee`: login, `none` for unassigned, or `*` for anyone assigned.
 * - `creator`: login of the author.
 * - `since`: ISO 8601 timestamp; only issues updated at or after it.
 * - `sort`: `created` (default), `updated` or `comments`; `direction`: `desc` (default) or
 *   `asc`.
 * - `per_page` defaults to 30 (max 100), `page` starts at 1.
 *
 * GitHub's list endpoint does not include comments or PR diffs; call `get_issue` and
 * `list_comments` for the discussion.
 */
export async function list_issues(
  args: {
    repo: string;
    state?: "open" | "closed" | "all";
    labels?: string | string[];
    assignee?: string;
    creator?: string;
    since?: string;
    sort?: "created" | "updated" | "comments";
    direction?: "asc" | "desc";
    per_page?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<IssueList> {
  const params = {
    state: args.state,
    labels: labelList(args.labels),
    assignee: args.assignee,
    creator: args.creator,
    since: args.since,
    sort: args.sort,
    direction: args.direction,
    per_page: args.per_page ?? 30,
    page: args.page,
  };
  const issues: Issue[] = await gh(ctx, "GET", `${repoPath(args.repo)}/issues`, { params });
  return { issues };
}

/**
 * Read one issue by number as `{ issue }`. GitHub's issue endpoints also serve pull requests:
 * pass a PR number and you get the PR's issue-shaped record (title, body, state, labels,
 * assignees, comment count) including its `pull_request` field, but not its diff, files or
 * review state.
 */
export async function get_issue(
  args: { repo: string; number: number },
  ctx: Ctx,
): Promise<{ issue: Issue }> {
  const path = `${repoPath(args.repo)}/issues/${positive(args.number, "number")}`;
  return { issue: await gh(ctx, "GET", path) };
}

/**
 * Open a new issue and return it as `{ issue }`; `issue.number` is what `get_issue`,
 * `list_comments` and `add_comment` take. `title` is required. `body` is markdown. `labels`
 * are label names that must already exist (unknown names are ignored or created depending on
 * the token's permissions), and `assignees` are logins with push access.
 */
export async function create_issue(
  args: { repo: string; title: string; body?: string; labels?: string[]; assignees?: string[] },
  ctx: Ctx,
): Promise<{ issue: Issue }> {
  const title = (args.title || "").trim();
  if (!title) throw new Error("title is required");
  const body: Record<string, unknown> = { title };
  if (args.body !== undefined) body.body = args.body;
  if (args.labels !== undefined) body.labels = args.labels;
  if (args.assignees !== undefined) body.assignees = args.assignees;
  const issue: Issue = await gh(ctx, "POST", `${repoPath(args.repo)}/issues`, { body });
  return { issue };
}

/**
 * Edit an issue (or a pull request's issue fields) and return the updated `{ issue }`. Only
 * the fields you pass change; pass at least one. `body: ""` clears the body, `labels: []`
 * removes all labels and `assignees: []` unassigns everyone. `state` is `open` or `closed`;
 * when closing, `state_reason` is `completed` (default) or `not_planned`, and `reopened` is
 * accepted when reopening. On a pull request number this is how to set its labels and
 * assignees; use `update_pr` for its base branch.
 */
export async function update_issue(
  args: {
    repo: string;
    number: number;
    title?: string;
    body?: string | null;
    state?: "open" | "closed";
    state_reason?: "completed" | "not_planned" | "reopened";
    labels?: string[] | null;
    assignees?: string[] | null;
  },
  ctx: Ctx,
): Promise<{ issue: Issue }> {
  const payload: Record<string, unknown> = {};
  if (args.title !== undefined) payload.title = args.title;
  if (args.body !== undefined) payload.body = args.body;
  if (args.state !== undefined) payload.state = args.state;
  if (args.state_reason !== undefined) payload.state_reason = args.state_reason;
  if (args.labels !== undefined) payload.labels = args.labels;
  if (args.assignees !== undefined) payload.assignees = args.assignees;
  if (Object.keys(payload).length === 0) {
    throw new Error("pass at least one of title, body, state, state_reason, labels or assignees");
  }
  const path = `${repoPath(args.repo)}/issues/${positive(args.number, "number")}`;
  const issue: Issue = await gh(ctx, "PATCH", path, { body: payload });
  return { issue };
}

/**
 * List the conversation comments on an issue OR pull request, as `{ comments }`, oldest
 * first. `number` is the number of either: a PR's conversation comments are served by
 * the same endpoint (inline review comments on diff lines come from `list_pr_review_comments`).
 * One page at a time: `per_page`
 * defaults to 30 (max 100), `page` starts at 1.
 */
export async function list_comments(
  args: { repo: string; number: number; per_page?: number; page?: number },
  ctx: Ctx,
): Promise<CommentList> {
  const path = `${repoPath(args.repo)}/issues/${positive(args.number, "number")}/comments`;
  const comments: Comment[] = await gh(ctx, "GET", path, {
    params: { per_page: args.per_page ?? 30, page: args.page },
  });
  return { comments };
}

/**
 * Post a comment and return it as `{ comment }`. `number` is an issue or pull request
 * number: commenting on a PR this way posts to the PR's conversation (not an inline review
 * comment). `body` is markdown and required. See `request` for editing or deleting a comment
 * (`PATCH`/`DELETE /repos/{owner}/{repo}/issues/comments/{comment_id}`).
 */
export async function add_comment(
  args: { repo: string; number: number; body: string },
  ctx: Ctx,
): Promise<{ comment: Comment }> {
  const body = args.body || "";
  if (!body.trim()) throw new Error("body is required");
  const path = `${repoPath(args.repo)}/issues/${positive(args.number, "number")}/comments`;
  const comment: Comment = await gh(ctx, "POST", path, { body: { body } });
  return { comment };
}
