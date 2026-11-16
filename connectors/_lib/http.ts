// Request helpers shared by connectors.
import type { Response } from "../centaur";

export type Params = Record<string, string | number | boolean | undefined | null>;

/** `a=1&b=2`, skipping undefined, null and empty values. */
export function encode(params: Params): string {
  return Object.entries(params)
    .filter(([, value]) => value !== undefined && value !== null && value !== "")
    .map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`)
    .join("&");
}

/** `?a=1&b=2`, or "" when there are no parameters. */
export function query(params: Params): string {
  const encoded = encode(params);
  return encoded ? `?${encoded}` : "";
}

/** The JSON body of a 2xx response; otherwise throws `<label> returned HTTP <status>: <body>`. */
export async function readJson(res: Response, label: string): Promise<any> {
  const text = await res.text();
  if (!res.ok) throw new Error(`${label} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return JSON.parse(text);
}
