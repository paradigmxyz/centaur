import type { Ctx } from "../centaur";
import { readJson } from "../_lib/http";

// Results are Linear's own GraphQL objects, passed through. The interfaces list
// the fields the queries select; Linear sends exactly those. The personal API
// key is injected by iron-proxy as the raw `Authorization` header value (Linear
// does not use a Bearer prefix for personal API keys).

const DEFAULT_GRAPHQL_ENDPOINT = "https://api.linear.app/graphql";
const UPLOADS_PREFIX = "https://uploads.linear.app/";
const DEFAULT_PAGE_SIZE = 100;
const MAX_PAGE_SIZE = 250;

/** One cursor-paginated Linear connection, as the API sends it. */
export interface LinearConnection<T> {
  nodes: T[];
  pageInfo?: PageInfo;
  [field: string]: unknown;
}

export interface PageInfo {
  hasNextPage: boolean;
  endCursor: string | null;
  startCursor?: string | null;
  [field: string]: unknown;
}

/** A Linear user, as referenced by assignee, lead, creator and comment authors. */
export interface User {
  id: string;
  name: string;
  email?: string;
  displayName?: string;
  active?: boolean;
  [field: string]: unknown;
}

export interface Team {
  id: string;
  name: string;
  /** Short uppercase key such as `ENG`; `issues` and `cycles` filter on it. */
  key: string;
  description?: string | null;
  [field: string]: unknown;
}

export interface WorkflowState {
  id: string;
  name: string;
  color?: string;
  /** `triage`, `backlog`, `unstarted`, `started`, `completed` or `canceled`. */
  type?: string;
  position?: number;
  team?: Team | null;
  [field: string]: unknown;
}

export interface IssueLabel {
  id: string;
  name: string;
  color?: string;
  /** `null` for a workspace-wide label. */
  team?: Team | null;
  [field: string]: unknown;
}

export interface Cycle {
  id: string;
  name?: string | null;
  number?: number;
  startsAt?: string;
  endsAt?: string;
  /** Percent complete, 0-100 (a number, not a fraction). */
  progress?: number;
  team?: Team | null;
  issues?: LinearConnection<IssueSummary>;
  [field: string]: unknown;
}

export interface ProjectMilestone {
  id: string;
  name: string;
  description?: string | null;
  targetDate?: string | null;
  progress?: number;
  project?: Project | null;
  createdAt?: string;
  updatedAt?: string;
  [field: string]: unknown;
}

export interface Project {
  id: string;
  name: string;
  description?: string | null;
  state?: string;
  /** Percent complete, 0-100 (a number, not a fraction). */
  progress?: number;
  startDate?: string | null;
  targetDate?: string | null;
  lead?: User | null;
  teams?: LinearConnection<Team>;
  projectMilestones?: LinearConnection<ProjectMilestone>;
  issues?: LinearConnection<IssueSummary>;
  status?: { id: string; name: string; type?: string; color?: string; [field: string]: unknown } | null;
  url?: string;
  [field: string]: unknown;
}

/** An issue as listed; `labels` is the connection Linear returns, not an array. */
export interface Issue {
  id: string;
  /** Human identifier such as `ENG-123`; accepted anywhere an issue id is. */
  identifier: string;
  title: string;
  description?: string | null;
  /** 0 = none, 1 = urgent, 2 = high, 3 = medium, 4 = low. */
  priority?: number;
  priorityLabel?: string;
  state?: WorkflowState | null;
  assignee?: User | null;
  team?: Team;
  project?: Project | null;
  projectMilestone?: ProjectMilestone | null;
  cycle?: Cycle | null;
  labels?: LinearConnection<IssueLabel>;
  dueDate?: string | null;
  createdAt?: string;
  updatedAt?: string;
  url?: string;
  [field: string]: unknown;
}

export interface IssueSummary {
  id: string;
  identifier: string;
  title: string;
  state?: { name: string; [field: string]: unknown } | null;
  [field: string]: unknown;
}

export interface Comment {
  id: string;
  body: string;
  createdAt?: string;
  updatedAt?: string;
  url?: string;
  issueId?: string;
  user?: User | null;
  [field: string]: unknown;
}

/** One issue with its comments, parent and sub-issues. */
export interface IssueDetail extends Issue {
  comments?: LinearConnection<Comment>;
  parent?: IssueSummary | null;
  children?: LinearConnection<IssueSummary>;
}

export interface IssueRelation {
  id: string;
  type: string;
  issue?: IssueSummary;
  relatedIssue?: IssueSummary;
  [field: string]: unknown;
}

/** Issue fields the ETL page functions select, including timestamps and creator. */
export interface EtlIssue {
  id: string;
  number: number;
  identifier: string;
  title: string;
  description?: string | null;
  url?: string;
  priority?: number;
  priorityLabel?: string;
  estimate?: number | null;
  dueDate?: string | null;
  team?: Team;
  project?: Project | null;
  cycle?: Cycle | null;
  state?: WorkflowState | null;
  assignee?: User | null;
  creator?: User | null;
  parent?: IssueSummary | null;
  createdAt?: string;
  updatedAt?: string;
  archivedAt?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
  canceledAt?: string | null;
  [field: string]: unknown;
}

export interface EtlProject {
  id: string;
  name: string;
  description?: string | null;
  slugId?: string;
  state?: string;
  status?: { id: string; name: string; type?: string; color?: string; [field: string]: unknown } | null;
  lead?: User | null;
  teams?: LinearConnection<Team>;
  createdAt?: string;
  updatedAt?: string;
  archivedAt?: string | null;
  completedAt?: string | null;
  canceledAt?: string | null;
  url?: string;
}

export interface EtlComment {
  id: string;
  body: string;
  url?: string;
  issueId?: string;
  projectId?: string | null;
  parentId?: string | null;
  user?: User | null;
  createdAt?: string;
  updatedAt?: string;
  archivedAt?: string | null;
  editedAt?: string | null;
  resolvedAt?: string | null;
}

type Variables = Record<string, unknown>;

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

async function gql(ctx: Ctx, query: string, variables: Variables = {}): Promise<any> {
  const headers: Record<string, string> = { "content-type": "application/json", accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.LINEAR_API_KEY) headers.authorization = ctx.env.LINEAR_API_KEY;
  const res = await ctx.fetch(ctx.env.LINEAR_GRAPHQL_URL || DEFAULT_GRAPHQL_ENDPOINT, {
    method: "POST",
    headers,
    body: JSON.stringify({ query, variables }),
  });
  const body = await readJson(res, "Linear");
  // Linear answers 200 with an `errors` array for rejected queries and mutations.
  if (body.errors?.length) {
    const message = body.errors[0]?.message ?? JSON.stringify(body.errors);
    throw new Error(`Linear API error: ${message}`);
  }
  return body.data ?? {};
}

/** Follow a connection from the root of `data` down `path`, then page it up to `limit` nodes. */
async function collect<T>(
  ctx: Ctx,
  query: string,
  path: string[],
  variables: Variables,
  limit: number,
): Promise<T[]> {
  if (limit <= 0) return [];
  const pageSize = Math.max(1, Math.min(DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE));
  const nodes: T[] = [];
  let cursor: string | null = null;
  for (;;) {
    const first = Math.min(pageSize, limit - nodes.length);
    const data = await gql(ctx, query, { ...variables, first, after: cursor });
    let connection: any = data;
    for (const key of path) {
      connection = connection ? connection[key] : undefined;
      if (!connection || typeof connection !== "object") return nodes;
    }
    const received: T[] = connection.nodes ?? [];
    nodes.push(...received);
    const pageInfo = connection.pageInfo ?? {};
    cursor = pageInfo.endCursor ?? null;
    if (!pageInfo.hasNextPage || !cursor || received.length === 0 || nodes.length >= limit) return nodes;
  }
}

function team_filter(team_key: string | undefined): { filter: string; declarations: string; variables: Variables } {
  if (!team_key) return { filter: "", declarations: "", variables: {} };
  return {
    filter: "filter: { team: { key: { eq: $teamKey } } }, ",
    declarations: "$teamKey: String",
    variables: { teamKey: team_key },
  };
}

const ME_QUERY = "query Me { viewer { id name email } }";

const TEAMS_QUERY = `
  query Teams($first: Int!, $after: String) {
    teams(first: $first, after: $after) {
      nodes { id name key description }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const USERS_QUERY = `
  query Users($first: Int!, $after: String) {
    users(first: $first, after: $after) {
      nodes { id name email displayName active }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const PROJECTS_QUERY = `
  query Projects($first: Int!, $after: String) {
    projects(first: $first, after: $after, orderBy: updatedAt) {
      nodes {
        id name description state progress startDate targetDate
        lead { id name }
        teams { nodes { id name key } }
        url
      }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const PROJECT_QUERY = `
  query Project($id: String!) {
    project(id: $id) {
      id name description state progress startDate targetDate
      lead { id name }
      teams { nodes { id name key } }
      projectMilestones { nodes { id name description targetDate progress } }
      issues {
        nodes { id identifier title state { name } projectMilestone { id name targetDate } }
      }
      url
    }
  }
`;

const MILESTONES_QUERY = `
  query ProjectMilestones($first: Int!, $after: String, $projectId: ID) {
    projectMilestones(
      filter: { project: { id: { eq: $projectId } } },
      first: $first,
      after: $after,
      orderBy: updatedAt
    ) {
      nodes { id name description targetDate progress project { id name } createdAt updatedAt }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const ANY_MILESTONES_QUERY = `
  query ProjectMilestones($first: Int!, $after: String) {
    projectMilestones(first: $first, after: $after, orderBy: updatedAt) {
      nodes { id name description targetDate progress project { id name } createdAt updatedAt }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const ISSUES_QUERY = `
  query Issues(
    $first: Int!,
    $after: String,
    $includeArchived: Boolean,
    {declarations}
  ) {
    issues(
      {filter}
      first: $first,
      after: $after,
      includeArchived: $includeArchived,
      orderBy: updatedAt
    ) {
      nodes {
        id identifier title description priority priorityLabel
        state { id name color }
        assignee { id name }
        team { id name key }
        project { id name }
        projectMilestone { id name targetDate }
        cycle { id name number }
        labels { nodes { id name color } }
        dueDate createdAt updatedAt url
      }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const ISSUE_QUERY = `
  query Issue($id: String!) {
    issue(id: $id) {
      id identifier title description priority priorityLabel
      state { id name color }
      assignee { id name }
      team { id name key }
      project { id name }
      projectMilestone { id name targetDate }
      cycle { id name number }
      labels { nodes { id name color } }
      comments { nodes { id body user { name } createdAt } }
      parent { id identifier title }
      children { nodes { id identifier title state { name } } }
      dueDate createdAt updatedAt url
    }
  }
`;

const CYCLES_QUERY = `
  query Cycles($first: Int!, $after: String, {declarations}) {
    cycles({filter}first: $first, after: $after, orderBy: updatedAt) {
      nodes {
        id name number startsAt endsAt progress
        team { id name key }
        issues { nodes { id identifier title state { name } } }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const STATES_QUERY = `
  query WorkflowStates($first: Int!, $after: String, {declarations}) {
    workflowStates({filter}first: $first, after: $after) {
      nodes { id name color type position team { id name key } }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const LABELS_QUERY = `
  query Labels($first: Int!, $after: String, {declarations}) {
    issueLabels({filter}first: $first, after: $after) {
      nodes { id name color team { id name key } }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const SEARCH_ISSUES_QUERY = `
  query SearchIssues($term: String!, $first: Int!, $after: String) {
    searchIssues(term: $term, first: $first, after: $after) {
      nodes {
        id identifier title
        state { name }
        assignee { name }
        team { key }
        project { id name }
        projectMilestone { id name targetDate }
        dueDate url
      }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const ISSUE_CREATE_MUTATION = `
  mutation IssueCreate($input: IssueCreateInput!) {
    issueCreate(input: $input) {
      success
      issue {
        id identifier title dueDate
        project { id name }
        projectMilestone { id name targetDate }
        url
      }
    }
  }
`;

const ISSUE_UPDATE_MUTATION = `
  mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {
    issueUpdate(id: $id, input: $input) {
      success
      issue {
        id identifier title dueDate
        state { name }
        project { id name }
        projectMilestone { id name targetDate }
        url
      }
    }
  }
`;

const COMMENT_CREATE_MUTATION = `
  mutation CommentCreate($input: CommentCreateInput!) {
    commentCreate(input: $input) {
      success
      comment { id body createdAt }
    }
  }
`;

const ADD_LABEL_MUTATION = `
  mutation AddLabel($id: String!, $labelId: String!) {
    issueAddLabel(id: $id, labelId: $labelId) { success }
  }
`;

const REMOVE_LABEL_MUTATION = `
  mutation RemoveLabel($id: String!, $labelId: String!) {
    issueRemoveLabel(id: $id, labelId: $labelId) { success }
  }
`;

const ISSUE_RELATION_MUTATION = `
  mutation IssueRelationCreate($input: IssueRelationCreateInput!) {
    issueRelationCreate(input: $input) {
      success
      issueRelation {
        id type
        issue { id identifier title }
        relatedIssue { id identifier title }
      }
    }
  }
`;

const ETL_PROJECTS_QUERY = `
  query LinearEtlProjects(
    $first: Int!,
    $after: String,
    $includeArchived: Boolean,
    {declarations}
  ) {
    projects(
      first: $first,
      after: $after,
      includeArchived: $includeArchived,
      orderBy: updatedAt,
      {filter}
    ) {
      nodes {
        id name description slugId state
        status { id name type color }
        lead { id name displayName email }
        teams { nodes { id name key } }
        createdAt updatedAt archivedAt completedAt canceledAt url
      }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const ETL_ISSUES_QUERY = `
  query LinearEtlIssues(
    $first: Int!,
    $after: String,
    $includeArchived: Boolean,
    {declarations}
  ) {
    issues(
      first: $first,
      after: $after,
      includeArchived: $includeArchived,
      orderBy: updatedAt,
      {filter}
    ) {
      nodes {
        id number identifier title description url priority priorityLabel estimate dueDate
        team { id name key }
        project { id name }
        cycle { id name number }
        state { id name type color }
        assignee { id name displayName email }
        creator { id name displayName email }
        parent { id identifier title }
        createdAt updatedAt archivedAt startedAt completedAt canceledAt
      }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

const ETL_COMMENTS_QUERY = `
  query LinearEtlComments(
    $first: Int!,
    $after: String,
    $includeArchived: Boolean,
    {declarations}
  ) {
    comments(
      first: $first,
      after: $after,
      includeArchived: $includeArchived,
      orderBy: updatedAt,
      {filter}
    ) {
      nodes {
        id body url issueId projectId parentId
        user { id name displayName email }
        createdAt updatedAt archivedAt editedAt resolvedAt
      }
      pageInfo { hasNextPage endCursor }
    }
  }
`;

function updated_after_scope(updated_after: string | undefined): { declarations: string; filter: string; variables: Variables } {
  if (!updated_after) return { declarations: "", filter: "", variables: {} };
  return {
    declarations: "$updatedAfter: DateTimeOrDuration",
    filter: "filter: { updatedAt: { gte: $updatedAfter } },",
    variables: { updatedAfter: updated_after },
  };
}

function page<T>(connection: any): LinearConnection<T> {
  return { nodes: connection?.nodes ?? [], pageInfo: connection?.pageInfo };
}

/** Resolve a team by key (case-insensitive) or id. */
async function team_by_ref(ctx: Ctx, ref: string): Promise<Team> {
  const teams = await collect<Team>(ctx, TEAMS_QUERY, ["teams"], {}, MAX_PAGE_SIZE);
  const needle = ref.trim();
  const match =
    teams.find((team) => team.id === needle) ??
    teams.find((team) => team.key.toLowerCase() === needle.toLowerCase());
  if (!match) {
    throw new Error(
      `team ${JSON.stringify(ref)} not found; workspace keys: ${teams.map((team) => team.key).join(", ")}`,
    );
  }
  return match;
}

/** Resolve a user by id, "me", email or name fragment (case-insensitive contains). */
async function user_by_ref(ctx: Ctx, ref: string): Promise<User> {
  const needle = ref.trim();
  if (needle.toLowerCase() === "me") {
    const viewer = (await gql(ctx, ME_QUERY)).viewer;
    if (!viewer) throw new Error("Linear returned no viewer for the current API key");
    return viewer;
  }
  if (UUID.test(needle)) {
    const user = (await gql(ctx, "query User($id: String!) { user(id: $id) { id name email displayName } }", { id: needle })).user;
    if (!user) throw new Error(`Linear user ${needle} not found`);
    return user;
  }
  const users = await collect<User>(ctx, USERS_QUERY, ["users"], {}, MAX_PAGE_SIZE);
  const lower = needle.toLowerCase();
  const match = users.find((user) =>
    [user.name, user.displayName, user.email].some((value) => value?.toLowerCase().includes(lower)),
  );
  if (!match) throw new Error(`user ${JSON.stringify(ref)} not found in the workspace`);
  return match;
}

/** Resolve a project by id, exact name, or partial name (case-insensitive). */
async function project_by_ref(ctx: Ctx, ref: string): Promise<Project> {
  const needle = ref.trim();
  if (UUID.test(needle)) {
    const project = (await gql(ctx, PROJECT_QUERY, { id: needle })).project;
    if (!project) throw new Error(`Linear project ${needle} not found`);
    return project;
  }
  const projects = await collect<Project>(ctx, PROJECTS_QUERY, ["projects"], {}, MAX_PAGE_SIZE);
  const lower = needle.toLowerCase();
  const match =
    projects.find((project) => project.name.toLowerCase() === lower) ??
    projects.find((project) => project.name.toLowerCase().includes(lower));
  if (!match) throw new Error(`project ${JSON.stringify(ref)} not found`);
  return match;
}

/** Resolve a milestone by id, or by exact name within one project. */
async function milestone_by_ref(ctx: Ctx, project_id: string, ref: string): Promise<ProjectMilestone> {
  const needle = ref.trim();
  if (UUID.test(needle)) return { id: needle, name: needle };
  const milestones = await collect<ProjectMilestone>(
    ctx, MILESTONES_QUERY, ["projectMilestones"], { projectId: project_id }, MAX_PAGE_SIZE,
  );
  const match = milestones.find((milestone) => milestone.name.toLowerCase() === needle.toLowerCase());
  if (!match) throw new Error(`milestone ${JSON.stringify(ref)} not found in project ${project_id}`);
  return match;
}

/** Resolve a workflow state by id or name fragment, scoped to a team. */
async function state_by_ref(ctx: Ctx, team_key: string, ref: string): Promise<WorkflowState> {
  const needle = ref.trim();
  const states = await collect<WorkflowState>(
    ctx, STATES_QUERY, ["workflowStates"], { teamKey: team_key }, MAX_PAGE_SIZE,
  );
  const match =
    states.find((state) => state.id === needle) ??
    states.find((state) => state.name.toLowerCase().includes(needle.toLowerCase()));
  if (!match) {
    throw new Error(
      `state ${JSON.stringify(ref)} not found in team ${team_key}; states: ${states.map((state) => state.name).join(", ")}`,
    );
  }
  return match;
}

/** Resolve label names to ids, preferring a team-scoped label over a workspace label of the same name. */
async function resolve_label_ids(ctx: Ctx, names: string[], team_key: string | undefined): Promise<string[]> {
  if (names.length === 0) return [];
  const query = `
    query Labels($names: [String!]) {
      issueLabels(filter: { name: { in: $names } }, first: 250) {
        nodes { id name team { key } }
      }
    }
  `;
  const nodes: { id: string; name: string; team?: { key?: string } | null }[] =
    (await gql(ctx, query, { names })).issueLabels?.nodes ?? [];

  const teamHits: Record<string, string[]> = {};
  const workspaceHits: Record<string, string[]> = {};
  for (const name of names) {
    teamHits[name] = [];
    workspaceHits[name] = [];
  }
  for (const node of nodes) {
    if (!(node.name in teamHits)) continue;
    if (!node.team) workspaceHits[node.name].push(node.id);
    else if (team_key && node.team.key === team_key) teamHits[node.name].push(node.id);
  }

  const resolved: string[] = [];
  const missing: string[] = [];
  const ambiguous: string[] = [];
  for (const name of names) {
    const source = teamHits[name].length ? teamHits[name] : workspaceHits[name];
    if (source.length === 0) missing.push(name);
    else if (source.length > 1) ambiguous.push(`${name} (${teamHits[name].length ? `team ${team_key}` : "workspace"})`);
    else resolved.push(source[0]);
  }
  if (missing.length) {
    throw new Error(
      `missing label(s): ${missing.join(", ")}. Create them in team ${team_key ?? "<workspace>"} or at the workspace level.`,
    );
  }
  if (ambiguous.length) {
    throw new Error(`ambiguous label(s): ${ambiguous.join(", ")}. Each must exist exactly once in its scope.`);
  }
  return resolved;
}

/**
 * The authenticated API key's user (`viewer`): id, name and email. Also a cheap check that the
 * key works.
 */
export async function me(args: {}, ctx: Ctx): Promise<{ viewer: User }> {
  const viewer = (await gql(ctx, ME_QUERY)).viewer;
  if (!viewer) throw new Error("Linear returned no viewer for the current API key");
  return { viewer };
}

/** Workspace teams with id, name, key (such as `ENG`) and description, up to `limit` (default 50). */
export async function teams(args: { limit?: number }, ctx: Ctx): Promise<{ teams: Team[] }> {
  const items = await collect<Team>(ctx, TEAMS_QUERY, ["teams"], {}, args.limit ?? 50);
  return { teams: items };
}

/**
 * Issues, most recently updated first, up to `limit` (default 50). Filters: `team_key` (`ENG`),
 * `assignee` (a name fragment, or `me`), `state` (a state-name fragment such as `progress`),
 * `project_id`, `project_milestone_id`; `include_archived` also returns archived issues. Each issue
 * carries state, assignee, team, project, milestone, cycle, labels, priority, due date and url.
 * Pagination beyond `limit` is not exposed: raise `limit` instead (100 issues per request).
 */
export async function issues(
  args: {
    team_key?: string;
    assignee?: string;
    state?: string;
    project_id?: string;
    project_milestone_id?: string;
    include_archived?: boolean;
    limit?: number;
  },
  ctx: Ctx,
): Promise<{ issues: Issue[] }> {
  const conditions: string[] = [];
  const variables: Variables = { includeArchived: args.include_archived ?? false };
  const declarations: string[] = [];
  if (args.team_key) {
    conditions.push("team: { key: { eq: $teamKey } }");
    declarations.push("$teamKey: String");
    variables.teamKey = args.team_key;
  }
  if (args.assignee) {
    if (args.assignee.trim().toLowerCase() === "me") {
      conditions.push("assignee: { isMe: { eq: true } }");
    } else {
      conditions.push("assignee: { name: { containsIgnoreCase: $assignee } }");
      declarations.push("$assignee: String");
      variables.assignee = args.assignee;
    }
  }
  if (args.state) {
    conditions.push("state: { name: { containsIgnoreCase: $stateName } }");
    declarations.push("$stateName: String");
    variables.stateName = args.state;
  }
  if (args.project_id) {
    conditions.push("project: { id: { eq: $projectId } }");
    declarations.push("$projectId: ID");
    variables.projectId = args.project_id;
  }
  if (args.project_milestone_id) {
    conditions.push("projectMilestone: { id: { eq: $projectMilestoneId } }");
    declarations.push("$projectMilestoneId: ID");
    variables.projectMilestoneId = args.project_milestone_id;
  }
  const query = ISSUES_QUERY.replace("{declarations}", declarations.join(",\n    ")).replace(
    "{filter}",
    conditions.length ? `filter: { ${conditions.join(", ")} }, ` : "",
  );
  const items = await collect<Issue>(ctx, query, ["issues"], variables, args.limit ?? 50);
  return { issues: items };
}

/**
 * One issue by id (UUID) or identifier (`ENG-123`, from a Linear URL or a mention), with comments,
 * parent and sub-issues.
 */
export async function issue(args: { id: string }, ctx: Ctx): Promise<{ issue: IssueDetail }> {
  const found = (await gql(ctx, ISSUE_QUERY, { id: args.id.trim() })).issue;
  if (!found) throw new Error(`Linear issue ${JSON.stringify(args.id)} not found`);
  return { issue: found };
}

/**
 * Download a Linear-hosted asset (an embedded screenshot or uploaded file) by its
 * `https://uploads.linear.app/...` URL and return the bytes base64-encoded with their mime type.
 * Only that host is accepted; results over the 10 MiB response limit fail.
 */
export async function fetch_asset(
  args: { url: string },
  ctx: Ctx,
): Promise<{ data: string; mime_type: string; byte_length: number }> {
  const url = args.url.trim();
  if (!url.startsWith(UPLOADS_PREFIX)) {
    throw new Error(`fetch_asset only retrieves ${UPLOADS_PREFIX}... URLs; got ${JSON.stringify(args.url)}`);
  }
  const headers: Record<string, string> = {};
  // Behind iron-proxy the key is injected for uploads.linear.app; locally it comes from the env.
  if (ctx.env.LINEAR_API_KEY) headers.authorization = ctx.env.LINEAR_API_KEY;
  const res = await ctx.fetch(url, { headers });
  if (!res.ok) throw new Error(`Linear asset returned HTTP ${res.status}: ${(await res.text()).slice(0, 1000)}`);
  const bytes = await res.bytes();
  return {
    data: ctx.base64.encode(bytes),
    mime_type: (res.headers["content-type"] ?? "application/octet-stream").split(";")[0].trim(),
    byte_length: bytes.length,
  };
}

/** Projects, most recently updated first, up to `limit` (default 50): state, progress, lead, teams, dates, url. */
export async function projects(args: { limit?: number }, ctx: Ctx): Promise<{ projects: Project[] }> {
  const items = await collect<Project>(ctx, PROJECTS_QUERY, ["projects"], {}, args.limit ?? 50);
  return { projects: items };
}

/**
 * One project by id (UUID) or name (exact first, then partial; case-insensitive), with its
 * milestones, issues, teams and description.
 */
export async function project(args: { id: string }, ctx: Ctx): Promise<{ project: Project }> {
  const found = await project_by_ref(ctx, args.id);
  const detail = (await gql(ctx, PROJECT_QUERY, { id: found.id })).project;
  if (!detail) throw new Error(`Linear project ${JSON.stringify(args.id)} not found`);
  return { project: detail };
}

/**
 * Project milestones, most recently updated first, up to `limit` (default 50). `project_id`
 * restricts to one project (ids come from `projects`).
 */
export async function project_milestones(
  args: { project_id?: string; limit?: number },
  ctx: Ctx,
): Promise<{ project_milestones: ProjectMilestone[] }> {
  const items = args.project_id
    ? await collect<ProjectMilestone>(ctx, MILESTONES_QUERY, ["projectMilestones"], { projectId: args.project_id }, args.limit ?? 50)
    : await collect<ProjectMilestone>(ctx, ANY_MILESTONES_QUERY, ["projectMilestones"], {}, args.limit ?? 50);
  return { project_milestones: items };
}

/**
 * Cycles, most recently updated first, up to `limit` (default 20). `team_key` restricts to one
 * team. Each cycle carries name, number, start/end dates, progress and its issues.
 */
export async function cycles(
  args: { team_key?: string; limit?: number },
  ctx: Ctx,
): Promise<{ cycles: Cycle[] }> {
  const scope = team_filter(args.team_key);
  const query = CYCLES_QUERY.replace("{declarations}", scope.declarations).replace("{filter}", scope.filter);
  const items = await collect<Cycle>(ctx, query, ["cycles"], scope.variables, args.limit ?? 20);
  return { cycles: items };
}

/**
 * Workflow states with their type (`triage`, `backlog`, `unstarted`, `started`, `completed`,
 * `canceled`) and position, up to 100. `team_key` restricts to one team; without it, every team's
 * states are returned (each carries its team).
 */
export async function workflow_states(
  args: { team_key?: string },
  ctx: Ctx,
): Promise<{ workflow_states: WorkflowState[] }> {
  const scope = team_filter(args.team_key);
  const query = STATES_QUERY.replace("{declarations}", scope.declarations).replace("{filter}", scope.filter);
  const items = await collect<WorkflowState>(ctx, query, ["workflowStates"], scope.variables, 100);
  return { workflow_states: items };
}

/**
 * Issue labels (name, color, and the owning team or none for workspace-wide), up to 100.
 * `team_key` restricts to one team's labels.
 */
export async function labels(
  args: { team_key?: string },
  ctx: Ctx,
): Promise<{ labels: IssueLabel[] }> {
  const scope = team_filter(args.team_key);
  const query = LABELS_QUERY.replace("{declarations}", scope.declarations).replace("{filter}", scope.filter);
  const items = await collect<IssueLabel>(ctx, query, ["issueLabels"], scope.variables, 100);
  return { labels: items };
}

/**
 * Workspace users with id, name, email, display name and active flag, up to `limit` (default 100).
 * `query` filters locally on name, display name or email (case-insensitive).
 */
export async function users(
  args: { query?: string; limit?: number },
  ctx: Ctx,
): Promise<{ users: User[] }> {
  const items = await collect<User>(ctx, USERS_QUERY, ["users"], {}, args.limit ?? 100);
  const needle = args.query?.trim().toLowerCase();
  if (!needle) return { users: items };
  return {
    users: items.filter((user) =>
      [user.name, user.displayName, user.email].some((value) => value?.toLowerCase().includes(needle)),
    ),
  };
}

/**
 * Full-text issue search over the workspace, most relevant first, up to `limit` (default 25).
 * Each hit carries identifier, title, state, assignee, team key, project, milestone, due date, url.
 */
export async function search_issues(
  args: { query: string; limit?: number },
  ctx: Ctx,
): Promise<{ issues: IssueSummary[] }> {
  const items = await collect<IssueSummary>(
    ctx, SEARCH_ISSUES_QUERY, ["searchIssues"], { term: args.query }, args.limit ?? 25,
  );
  return { issues: items };
}

/**
 * Write: create an issue. `team` is a team key (`ENG`) or id. `assignee`, `state`, `project` and
 * `milestone` accept an id or a human name (assignee also `me`); `parent` is an identifier such as
 * `ENG-123`. `priority` is 0 (none), 1 (urgent), 2 (high), 3 (medium) or 4 (low); `due_date` is
 * `YYYY-MM-DD`. `state`/`milestone` need `team`/`project` to resolve. Throws when Linear reports
 * the mutation did not succeed.
 */
export async function create_issue(
  args: {
    title: string;
    team: string;
    description?: string;
    assignee?: string;
    state?: string;
    priority?: number;
    project?: string;
    milestone?: string;
    parent?: string;
    due_date?: string;
  },
  ctx: Ctx,
): Promise<{ issue: IssueSummary }> {
  const team = await team_by_ref(ctx, args.team);
  const input: Variables = { title: args.title, teamId: team.id };
  if (args.description) input.description = args.description;
  if (args.state) input.stateId = (await state_by_ref(ctx, team.key, args.state)).id;
  if (args.assignee) input.assigneeId = (await user_by_ref(ctx, args.assignee)).id;
  if (args.priority !== undefined) input.priority = args.priority;
  let project_id: string | undefined;
  if (args.project) {
    project_id = (await project_by_ref(ctx, args.project)).id;
    input.projectId = project_id;
  }
  if (args.milestone) {
    if (!project_id) throw new Error("milestone requires project when creating an issue");
    input.projectMilestoneId = (await milestone_by_ref(ctx, project_id, args.milestone)).id;
  }
  if (args.parent) {
    const parent = (await gql(ctx, ISSUE_QUERY, { id: args.parent.trim() })).issue;
    if (!parent) throw new Error(`parent issue ${JSON.stringify(args.parent)} not found`);
    input.parentId = parent.id;
  }
  if (args.due_date) input.dueDate = args.due_date;

  const payload = (await gql(ctx, ISSUE_CREATE_MUTATION, { input })).issueCreate;
  if (!payload?.success) throw new Error(`Linear issueCreate failed: ${JSON.stringify(payload ?? null)}`);
  return { issue: payload.issue };
}

/**
 * Write: update an issue (`ENG-123` or id). Only the arguments given are changed. `state` is a
 * state-name fragment or id in the issue's team; `assignee` an id, name or `me`; `project` a
 * project name or id; `milestone` a milestone name or id in the issue's project and
 * `clear_milestone` removes it (they are mutually exclusive). `priority` is 0-4, `due_date` is
 * `YYYY-MM-DD`. Throws when Linear reports the mutation did not succeed.
 */
export async function update_issue(
  args: {
    id: string;
    title?: string;
    description?: string;
    state?: string;
    assignee?: string;
    priority?: number;
    project?: string;
    milestone?: string;
    clear_milestone?: boolean;
    due_date?: string;
  },
  ctx: Ctx,
): Promise<{ issue: IssueSummary }> {
  if (args.milestone && args.clear_milestone) {
    throw new Error("milestone and clear_milestone are mutually exclusive");
  }
  const input: Variables = {};
  let current: IssueDetail | undefined;
  const load_current = async (): Promise<IssueDetail> => {
    const loaded = current;
    if (loaded) return loaded;
    const found = (await gql(ctx, ISSUE_QUERY, { id: args.id.trim() })).issue;
    if (!found) throw new Error(`Linear issue ${JSON.stringify(args.id)} not found`);
    current = found;
    return found;
  };

  if (args.title) input.title = args.title;
  if (args.description) input.description = args.description;
  if (args.state) {
    const issue_now = await load_current();
    const team_key = issue_now.team?.key;
    if (!team_key) throw new Error(`issue ${args.id} has no team, so state ${JSON.stringify(args.state)} cannot be resolved`);
    input.stateId = (await state_by_ref(ctx, team_key, args.state)).id;
  }
  if (args.assignee) input.assigneeId = (await user_by_ref(ctx, args.assignee)).id;
  if (args.priority !== undefined) input.priority = args.priority;
  let project_id: string | undefined;
  if (args.project) {
    project_id = (await project_by_ref(ctx, args.project)).id;
    input.projectId = project_id;
  }
  if (args.milestone) {
    const issue_now = await load_current();
    const scope = project_id ?? issue_now.project?.id;
    if (!scope) throw new Error(`issue ${args.id} belongs to no project, so a milestone cannot be set`);
    input.projectMilestoneId = (await milestone_by_ref(ctx, scope, args.milestone)).id;
  } else if (args.clear_milestone) {
    input.projectMilestoneId = null;
  }
  if (args.due_date) input.dueDate = args.due_date;

  const payload = (await gql(ctx, ISSUE_UPDATE_MUTATION, { id: args.id.trim(), input })).issueUpdate;
  if (!payload?.success) throw new Error(`Linear issueUpdate failed: ${JSON.stringify(payload ?? null)}`);
  return { issue: payload.issue };
}

/**
 * Write: add a Markdown comment to an issue (`ENG-123` or id). Returns the new comment's id, body
 * and creation time. Throws when Linear reports the mutation did not succeed.
 */
export async function add_comment(
  args: { id: string; body: string },
  ctx: Ctx,
): Promise<{ comment: { id: string; body: string; createdAt?: string; [field: string]: unknown } }> {
  const payload = (await gql(ctx, COMMENT_CREATE_MUTATION, {
    input: { issueId: args.id.trim(), body: args.body },
  })).commentCreate;
  if (!payload?.success) throw new Error(`Linear commentCreate failed: ${JSON.stringify(payload ?? null)}`);
  return { comment: payload.comment };
}

/**
 * Write: add one label, by name, to an issue without touching its other labels. Pass `team_key`
 * to pick a team-scoped label when a workspace label shares the name. Throws if the label does not
 * exist or is ambiguous in its scope.
 */
export async function add_label(
  args: { id: string; label: string; team_key?: string },
  ctx: Ctx,
): Promise<{ success: boolean }> {
  const [label_id] = await resolve_label_ids(ctx, [args.label], args.team_key);
  const payload = (await gql(ctx, ADD_LABEL_MUTATION, { id: args.id.trim(), labelId: label_id })).issueAddLabel;
  if (!payload?.success) throw new Error(`Linear issueAddLabel failed: ${JSON.stringify(payload ?? null)}`);
  return { success: true };
}

/**
 * Write: remove one label, by name, from an issue without touching its other labels. Succeeds even
 * if the label is not currently applied; throws if no label by that name exists in the chosen scope.
 */
export async function remove_label(
  args: { id: string; label: string; team_key?: string },
  ctx: Ctx,
): Promise<{ success: boolean }> {
  const [label_id] = await resolve_label_ids(ctx, [args.label], args.team_key);
  const payload = (await gql(ctx, REMOVE_LABEL_MUTATION, { id: args.id.trim(), labelId: label_id })).issueRemoveLabel;
  if (!payload?.success) throw new Error(`Linear issueRemoveLabel failed: ${JSON.stringify(payload ?? null)}`);
  return { success: true };
}

/**
 * Write: link two issues. `relation` is `blocks` (the first issue blocks the second), `blocked-by`
 * (the first is blocked by the second, arguments swapped before the call), `related` or
 * `duplicate`. Issues are identifiers (`ENG-123`) or ids. Returns the created relation with both
 * issues. Throws when Linear reports the mutation did not succeed.
 */
export async function create_issue_relation(
  args: { issue: string; related_issue: string; relation?: "blocks" | "blocked-by" | "related" | "duplicate" },
  ctx: Ctx,
): Promise<{ relation: IssueRelation }> {
  const relation = args.relation ?? "blocks";
  if (!["blocks", "blocked-by", "related", "duplicate"].includes(relation)) {
    throw new Error(`relation must be blocks, blocked-by, related or duplicate; got ${JSON.stringify(relation)}`);
  }
  const swapped = relation === "blocked-by";
  const issue_id = (swapped ? args.related_issue : args.issue).trim();
  const related_issue_id = (swapped ? args.issue : args.related_issue).trim();
  const type = swapped ? "blocks" : relation;
  const payload = (await gql(ctx, ISSUE_RELATION_MUTATION, {
    input: { issueId: issue_id, relatedIssueId: related_issue_id, type },
  })).issueRelationCreate;
  if (!payload?.success) throw new Error(`Linear issueRelationCreate failed: ${JSON.stringify(payload ?? null)}`);
  return { relation: payload.issueRelation };
}

/**
 * One page of projects for syncing, most recently updated first, as Linear's connection
 * (`nodes` + `pageInfo`, not unwrapped). `page_size` is at most 250 (default 100); pass
 * `pageInfo.endCursor` as `cursor` for the next page. `updated_after` is an ISO 8601 timestamp.
 * Fields include status, lead with email, teams and created/updated/completed timestamps.
 */
export async function list_etl_projects(
  args: { page_size?: number; cursor?: string; updated_after?: string; include_archived?: boolean },
  ctx: Ctx,
): Promise<{ projects: LinearConnection<EtlProject> }> {
  return { projects: await etl_page<EtlProject>(ctx, ETL_PROJECTS_QUERY, "projects", args) };
}

/**
 * One page of issues for syncing, most recently updated first, as Linear's connection (`nodes` +
 * `pageInfo`). `page_size` is at most 250 (default 100); pass `pageInfo.endCursor` as `cursor`.
 * `updated_after` is an ISO 8601 timestamp. Fields include description, estimate, state type,
 * assignee/creator with email, parent and lifecycle timestamps.
 */
export async function list_etl_issues(
  args: { page_size?: number; cursor?: string; updated_after?: string; include_archived?: boolean },
  ctx: Ctx,
): Promise<{ issues: LinearConnection<EtlIssue> }> {
  return { issues: await etl_page<EtlIssue>(ctx, ETL_ISSUES_QUERY, "issues", args) };
}

/**
 * One page of comments for syncing, most recently updated first, as Linear's connection (`nodes` +
 * `pageInfo`). `page_size` is at most 250 (default 100); pass `pageInfo.endCursor` as `cursor`.
 * `updated_after` is an ISO 8601 timestamp. Each comment carries `issueId`, `projectId`, author and
 * edit/resolve timestamps.
 */
export async function list_etl_comments(
  args: { page_size?: number; cursor?: string; updated_after?: string; include_archived?: boolean },
  ctx: Ctx,
): Promise<{ comments: LinearConnection<EtlComment> }> {
  return { comments: await etl_page<EtlComment>(ctx, ETL_COMMENTS_QUERY, "comments", args) };
}

async function etl_page<T>(
  ctx: Ctx,
  template: string,
  key: string,
  args: { page_size?: number; cursor?: string; updated_after?: string; include_archived?: boolean },
): Promise<LinearConnection<T>> {
  const scope = updated_after_scope(args.updated_after);
  const query = template.replace("{declarations}", scope.declarations).replace("{filter}", scope.filter);
  const variables: Variables = {
    first: Math.max(1, Math.min(args.page_size ?? DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE)),
    after: args.cursor,
    includeArchived: args.include_archived ?? true,
    ...scope.variables,
  };
  return page<T>((await gql(ctx, query, variables))[key]);
}
