// Non-component helpers shared by the skills and SSH key pages.
import { ApiError, errorMessage, humanize } from "@/lib/api";

import type { SkillTab } from "../api";

export function skillsTabPath(tab: SkillTab) {
  return tab === "mine" ? "/console/skills/mine" : "/console/skills";
}

/** Every 422 message as "Field message" (legacy `errors.full_messages`). */
export function validationMessages(error: unknown): string[] {
  if (error instanceof ApiError && error.status === 422) {
    const messages = Object.entries(error.fieldErrors)
      .filter(([key]) => key !== "valid")
      .flatMap(([key, list]) => list.map((message) => (key === "base" ? message : `${humanize(key)} ${message}`)));
    if (messages.length > 0) return messages;
  }
  return [errorMessage(error)];
}

/** Every validation message as one sentence (legacy `full_messages.to_sentence`). */
export function toSentence(messages: string[]): string {
  if (messages.length <= 1) return messages[0] ?? "";
  return `${messages.slice(0, -1).join(", ")} and ${messages.at(-1)}`;
}

export const INTEGRATIONS_PATH = "/console/integrations";

/** "2026-06-01 10:00 UTC" (legacy strftime "%Y-%m-%d %H:%M UTC"). */
export function formatUtcMinute(iso: string | null): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return `${date.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}
