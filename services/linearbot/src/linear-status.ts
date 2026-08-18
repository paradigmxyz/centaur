import type { JsonObject, LinearRawRequestClient } from "./types";
import { isJsonObject, stringValue } from "./utils";

// Linear delta (no slackbotv2 analog): workflow-status plumbing for DELEGATED
// issues. Delegation gives the agent "ownership" of an issue, so the issue's
// status should reflect its progress: the bot deterministically moves the
// issue to a started state when it kicks off work, and the agent signals the
// terminal status itself — either directly via the sandbox `linear` tool, or
// through a `Linear-Status: …` marker line in its final answer that the bot
// applies as a backstop. Mention-only sessions never move status: the agent
// only owns issues delegated to it.

/** Terminal status the agent can signal in its final answer. */
export type LinearStatusMarker = "done" | "in_progress" | "todo";

export type LinearWorkflowState = {
  id: string;
  name: string;
  position: number;
  /** Linear state type: triage|backlog|unstarted|started|completed|canceled */
  type: string;
};

export type LinearIssueStatus = {
  delegateId?: string;
  stateId?: string;
  stateName?: string;
  stateType?: string;
  states: LinearWorkflowState[];
};

const ISSUE_STATUS_QUERY = `
  query LinearbotIssueStatus($issueId: String!) {
    issue(id: $issueId) {
      id
      delegate { id }
      state { id name type }
      team {
        states {
          nodes { id name position type }
        }
      }
    }
  }
`;

const ISSUE_STATE_UPDATE_MUTATION = `
  mutation LinearbotIssueStateUpdate($issueId: String!, $stateId: String!) {
    issueUpdate(id: $issueId, input: { stateId: $stateId }) {
      success
    }
  }
`;

type IssueStatusQueryData = {
  issue?: {
    delegate?: { id?: unknown } | null;
    state?: { id?: unknown; name?: unknown; type?: unknown } | null;
    team?: { states?: { nodes?: unknown } | null } | null;
  } | null;
};

/**
 * Fetches the issue's delegate, current workflow state, and the team's state
 * catalog in one query. Returns null when the issue (or client) is
 * unavailable; never throws on malformed data.
 */
export async function fetchIssueStatus(
  client: LinearRawRequestClient,
  issueId: string,
): Promise<LinearIssueStatus | null> {
  if (!client.client?.rawRequest) return null;
  const response = await client.client.rawRequest<IssueStatusQueryData>(
    ISSUE_STATUS_QUERY,
    { issueId },
  );
  const issue = response.data?.issue;
  if (!issue) return null;
  return {
    delegateId: stringValue(issue.delegate?.id),
    stateId: stringValue(issue.state?.id),
    stateName: stringValue(issue.state?.name),
    stateType: stringValue(issue.state?.type),
    states: workflowStates(issue.team?.states?.nodes),
  };
}

export async function updateIssueState(
  client: LinearRawRequestClient,
  issueId: string,
  stateId: string,
): Promise<void> {
  if (!client.client?.rawRequest) return;
  await client.client.rawRequest(ISSUE_STATE_UPDATE_MUTATION, {
    issueId,
    stateId,
  });
}

function workflowStates(nodes: unknown): LinearWorkflowState[] {
  if (!Array.isArray(nodes)) return [];
  const states: LinearWorkflowState[] = [];
  for (const node of nodes) {
    if (!isJsonObject(node)) continue;
    const id = stringValue(node.id);
    const type = stringValue(node.type);
    if (!id || !type) continue;
    states.push({
      id,
      name: stringValue(node.name) ?? "",
      position: typeof node.position === "number" ? node.position : 0,
      type,
    });
  }
  return states;
}

const MARKER_TARGET_TYPES: Record<LinearStatusMarker, string> = {
  done: "completed",
  in_progress: "started",
  todo: "unstarted",
};

const REVIEW_STATE_NAME_PATTERN = /\breview\b/i;

// State types it is safe to move OUT of when kicking off work. Started,
// completed, and canceled issues are never touched: a human (or the agent
// itself) put them there deliberately.
const KICKOFF_SOURCE_TYPES = new Set(["triage", "backlog", "unstarted"]);

/** First (lowest-position) workflow state of the given type, if any. */
export function pickWorkflowState(
  states: LinearWorkflowState[],
  type: string,
): LinearWorkflowState | undefined {
  return states
    .filter((state) => state.type === type)
    .sort((a, b) => a.position - b.position)[0];
}

/**
 * The state to move a delegated issue to when the agent kicks off work, or
 * undefined when no move should happen (already started/completed, or the
 * team has no started state).
 */
export function kickoffTargetState(
  status: LinearIssueStatus,
): LinearWorkflowState | undefined {
  if (!status.stateType || !KICKOFF_SOURCE_TYPES.has(status.stateType)) {
    return undefined;
  }
  return pickWorkflowState(status.states, "started");
}

/**
 * The state a `Linear-Status:` marker maps to, or undefined when the issue is
 * already in a state of the requested type (or the team has none).
 */
export function markerTargetState(
  status: LinearIssueStatus,
  marker: LinearStatusMarker,
): LinearWorkflowState | undefined {
  if (marker === "done" && isReviewState(status)) return undefined;
  const targetType = MARKER_TARGET_TYPES[marker];
  if (status.stateType === targetType) return undefined;
  return pickWorkflowState(status.states, targetType);
}

function isReviewState(status: LinearIssueStatus): boolean {
  return REVIEW_STATE_NAME_PATTERN.test(status.stateName ?? "");
}

const STATUS_MARKER_PATTERN =
  /^[ \t]*linear-status:[ \t]*(done|in[-_ ]?progress|todo)[ \t]*$/gim;

/**
 * Extracts the agent's terminal `Linear-Status: …` marker line from its final
 * answer (the last occurrence wins) and strips every marker line from the
 * text posted to Linear.
 */
export function extractStatusMarker(text: string): {
  marker?: LinearStatusMarker;
  text: string;
} {
  let marker: LinearStatusMarker | undefined;
  const cleaned = text.replace(STATUS_MARKER_PATTERN, (_match, value) => {
    const normalized = String(value).toLowerCase().replace(/[-_ ]/g, "");
    marker =
      normalized === "done"
        ? "done"
        : normalized === "todo"
          ? "todo"
          : "in_progress";
    return "";
  });
  if (!marker) return { text };
  return { marker, text: cleaned.replace(/\n{3,}/g, "\n\n").trim() };
}

/** Trace payload for status-change logs. */
export function statusTraceFields(
  issueId: string,
  state: LinearWorkflowState,
): JsonObject {
  return { issue_id: issueId, state_id: state.id, state_name: state.name };
}
