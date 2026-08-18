import type { LinearRawRequestClient } from "./types";
import { stringValue } from "./utils";

// Centaur-forward model: mirror discordbot's reaction ack. The triggering
// comment gets an instant 👀 while the bot works, swapped for ✅ (or ❌ on
// failure) when the turn settles. Linear reactions are removed by id (not by
// emoji), so the create captures the 👀 reaction's id for the later delete.

export const REACTION_WORKING = "👀";
export const REACTION_DONE = "✅";
export const REACTION_FAILED = "❌";

const REACTION_CREATE_MUTATION = `
  mutation LinearbotReactionCreate($commentId: String!, $emoji: String!) {
    reactionCreate(input: { commentId: $commentId, emoji: $emoji }) {
      reaction { id }
    }
  }
`;

const REACTION_DELETE_MUTATION = `
  mutation LinearbotReactionDelete($id: String!) {
    reactionDelete(id: $id) {
      success
    }
  }
`;

type ReactionCreateData = {
  reactionCreate?: { reaction?: { id?: unknown } | null } | null;
};

/** Adds an emoji reaction to a comment; returns the reaction id (for removal). */
export async function addCommentReaction(
  client: LinearRawRequestClient,
  commentId: string,
  emoji: string,
): Promise<string | undefined> {
  if (!client.client?.rawRequest) return undefined;
  const response = await client.client.rawRequest<ReactionCreateData>(
    REACTION_CREATE_MUTATION,
    { commentId, emoji },
  );
  return stringValue(response.data?.reactionCreate?.reaction?.id);
}

/** Removes a previously added reaction by its id. */
export async function removeCommentReaction(
  client: LinearRawRequestClient,
  reactionId: string,
): Promise<void> {
  if (!client.client?.rawRequest) return;
  await client.client.rawRequest(REACTION_DELETE_MUTATION, { id: reactionId });
}
