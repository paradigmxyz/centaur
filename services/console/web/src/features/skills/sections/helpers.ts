import { ApiError, errorMessage, humanize } from "@/lib/api";

import type { SkillTab } from "../api";

export function skillsTabPath(tab: SkillTab) {
  return tab === "mine" ? "/console/skills/mine" : "/console/skills";
}

export function validationMessages(error: unknown): string[] {
  if (error instanceof ApiError && error.status === 422) {
    const messages = Object.entries(error.fieldErrors)
      .filter(([key]) => key !== "valid")
      .flatMap(([key, list]) => list.map((message) => (key === "base" ? message : `${humanize(key)} ${message}`)));
    if (messages.length > 0) return messages;
  }
  return [errorMessage(error)];
}

export function toSentence(messages: string[]): string {
  if (messages.length <= 1) return messages[0] ?? "";
  return `${messages.slice(0, -1).join(", ")} and ${messages.at(-1)}`;
}

export const INTEGRATIONS_PATH = "/console/integrations";

export function formatUtcMinute(iso: string | null): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return `${date.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}
