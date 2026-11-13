// Narrowing upstream JSON to the types connectors return.

/** An upstream JSON object whose shape the connector maps by hand. */
export type Raw = Record<string, any>;

/** A non-empty string, or null. */
export const str = (value: unknown): string | null =>
  typeof value === "string" && value !== "" ? value : null;

export const num = (value: unknown): number | null => (typeof value === "number" ? value : null);

export const bool = (value: unknown): boolean | null => (typeof value === "boolean" ? value : null);
