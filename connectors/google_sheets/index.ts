import type { Ctx, Response } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Google Sheets v4 resources as the API sends them (`camelCase`).
// Authentication is injected on the wire by iron-proxy's oauth_token transform;
// `GOOGLE_ACCESS_TOKEN` is a local-testing fallback only.

const DEFAULT_SHEETS_BASE = "https://sheets.googleapis.com/v4";
const DEFAULT_DRIVE_BASE = "https://www.googleapis.com/drive/v3";

/** A cell value as written to a sheet; `null` writes an empty cell. */
export type CellValue = string | number | boolean | null;

/** A table row for `write_table`: keyed by header, or positional. */
export type TableRow = Record<string, CellValue> | CellValue[];

/** Spreadsheet metadata: title, locale, time zone, url and its tabs. */
export interface Spreadsheet {
  spreadsheetId: string;
  properties?: SpreadsheetProperties;
  sheets?: Sheet[];
  spreadsheetUrl?: string;
  [field: string]: unknown;
}

/** Spreadsheet-level properties, including `timeZone` and `locale`. */
export interface SpreadsheetProperties {
  title?: string;
  locale?: string;
  timeZone?: string;
  autoRecalc?: string;
  [field: string]: unknown;
}

/** One tab and its properties. */
export interface Sheet {
  properties?: SheetProperties;
  [field: string]: unknown;
}

/** A tab's identity: `sheetId` (the `gid` in a URL), `title` used in A1 ranges, `index`. */
export interface SheetProperties {
  sheetId?: number;
  title?: string;
  index?: number;
  sheetType?: string;
  gridProperties?: {
    rowCount?: number;
    columnCount?: number;
    frozenRowCount?: number;
    frozenColumnCount?: number;
  };
  [field: string]: unknown;
}

/** A range of values, one array per row. */
export interface ValueRange {
  /** A1 range the values actually cover, such as `Sheet1!A1:C3`. */
  range?: string;
  /** `ROWS` (default) or `COLUMNS`. */
  majorDimension?: string;
  /** Values as strings (`RAW`) or as Sheets parsed them (`USER_ENTERED` reads). */
  values?: CellValue[][];
  [field: string]: unknown;
}

/** The outcome of a values write. */
export interface UpdateValuesResponse {
  spreadsheetId?: string;
  updatedRange?: string;
  updatedRows?: number;
  updatedColumns?: number;
  updatedCells?: number;
  updatedData?: ValueRange;
  [field: string]: unknown;
}

/** The outcome of a multi-range values write. */
export interface BatchUpdateValuesResponse {
  spreadsheetId?: string;
  totalUpdatedRows?: number;
  totalUpdatedColumns?: number;
  totalUpdatedCells?: number;
  totalUpdatedSheets?: number;
  responses?: UpdateValuesResponse[];
  [field: string]: unknown;
}

interface RequestOptions {
  method?: string;
  body?: unknown;
}

function sheetsBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_SHEETS_API_URL || DEFAULT_SHEETS_BASE;
}

function driveBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_DRIVE_API_URL || DEFAULT_DRIVE_BASE;
}

/** iron-proxy adds the bearer token on the wire; locally the env may hold one. */
function authHeaders(ctx: Ctx): Record<string, string> {
  const headers: Record<string, string> = {};
  if (ctx.env.GOOGLE_ACCESS_TOKEN) headers.authorization = `Bearer ${ctx.env.GOOGLE_ACCESS_TOKEN}`;
  return headers;
}

async function send(ctx: Ctx, url: string, options: RequestOptions = {}): Promise<Response> {
  const headers: Record<string, string> = { accept: "application/json", ...authHeaders(ctx) };
  if (options.body !== undefined) headers["content-type"] = "application/json";
  return ctx.fetch(url, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
}

async function sheetsRequest(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<any> {
  return readJson(await send(ctx, sheetsBase(ctx) + path, options), "Google Sheets");
}

/** Accept a spreadsheet id or a Sheets/Drive URL and return the id it contains. */
function spreadsheetId(value: string): string {
  if (!value.startsWith("http://") && !value.startsWith("https://")) return value;
  const path = /\/d\/([a-zA-Z0-9_-]+)/.exec(value);
  if (path) return path[1];
  const byQuery = /[?&]id=([a-zA-Z0-9_-]+)/.exec(value);
  if (byQuery) return byQuery[1];
  throw new Error(`Could not extract a spreadsheet id from URL: ${value}`);
}

/** Escape a tab title for A1 notation: `'My sheet'`. */
function quoteSheetTitle(title: string): string {
  return `'${title.replace(/'/g, "''")}'`;
}

/**
 * Spreadsheet metadata: title, locale, time zone, url, and every tab with its `sheetId`, title,
 * position and grid size. Use a tab title in A1 ranges (`'Tab name'!A1:D10`) and `sheetId` as the
 * `gid` in a direct URL.
 */
export async function get_spreadsheet(
  args: { spreadsheet_id: string; include_grid_data?: boolean },
  ctx: Ctx,
): Promise<{ spreadsheet: Spreadsheet }> {
  const id = spreadsheetId(args.spreadsheet_id);
  const spreadsheet = await sheetsRequest(
    ctx,
    `/spreadsheets/${encodeURIComponent(id)}${query({
      includeGridData: args.include_grid_data === true,
    })}`,
  );
  return { spreadsheet };
}

/**
 * Create a spreadsheet, optionally writing `content` (rows of cells) at A1 of the first tab and
 * creating it inside `folder_id` (a folder or shared drive id, via Drive). Returns the spreadsheet
 * id, title and editor URL.
 */
export async function create(
  args: { title: string; content?: CellValue[][]; folder_id?: string },
  ctx: Ctx,
): Promise<{ spreadsheet_id: string; title: string; url: string }> {
  let id: string;
  let title: string;
  if (args.folder_id) {
    const created = await readJson(
      await send(
        ctx,
        `${driveBase(ctx)}/files${query({ fields: "id,name", supportsAllDrives: true })}`,
        {
          body: {
            name: args.title,
            mimeType: "application/vnd.google-apps.spreadsheet",
            parents: [args.folder_id],
          },
        },
      ),
      "Google Drive",
    );
    id = created.id;
    title = created.name ?? args.title;
  } else {
    const spreadsheet = await sheetsRequest(ctx, "/spreadsheets", {
      body: { properties: { title: args.title } },
    });
    id = spreadsheet.spreadsheetId;
    title = spreadsheet.properties?.title ?? args.title;
  }
  if (args.content && args.content.length > 0) {
    await sheetsRequest(
      ctx,
      `/spreadsheets/${encodeURIComponent(id)}/values/A1${query({ valueInputOption: "RAW" })}`,
      { method: "PUT", body: { values: args.content } },
    );
  }
  return { spreadsheet_id: id, title, url: `https://docs.google.com/spreadsheets/d/${id}/edit` };
}

/**
 * Add a worksheet tab with `title`, at zero-based `index` (default: last). Returns the new tab's
 * properties (`sheetId`, `title`, `index`, `gridProperties`) and the tab's direct URL.
 */
export async function add_tab(
  args: { spreadsheet_id: string; title: string; index?: number },
  ctx: Ctx,
): Promise<{ spreadsheet_id: string; properties: SheetProperties; url: string }> {
  const id = spreadsheetId(args.spreadsheet_id);
  const properties: Record<string, unknown> = { title: args.title };
  if (args.index !== undefined) properties.index = args.index;
  const result = await sheetsRequest(ctx, `/spreadsheets/${encodeURIComponent(id)}:batchUpdate`, {
    body: { requests: [{ addSheet: { properties } }] },
  });
  const added: SheetProperties = result.replies?.[0]?.addSheet?.properties ?? { title: args.title };
  const url = added.sheetId === undefined
    ? `https://docs.google.com/spreadsheets/d/${id}/edit`
    : `https://docs.google.com/spreadsheets/d/${id}/edit#gid=${added.sheetId}`;
  return { spreadsheet_id: id, properties: added, url };
}

/**
 * Read one A1 range (default `A1:Z1000`; `'Tab name'!A1:D10` or just `A1:D10` for the first tab) as
 * `values`: one array per row, empty trailing cells omitted, formatted strings unless
 * `value_render_option` says otherwise. `major_dimension` (`ROWS` or `COLUMNS`) picks the nesting.
 * Whole-column ranges on large sheets are expensive: bound them with rows.
 */
export async function read_range(
  args: {
    spreadsheet_id: string;
    range?: string;
    value_render_option?: string;
    major_dimension?: string;
  },
  ctx: Ctx,
): Promise<{ value_range: ValueRange }> {
  const id = spreadsheetId(args.spreadsheet_id);
  const range = args.range ?? "A1:Z1000";
  const valueRange = await sheetsRequest(
    ctx,
    `/spreadsheets/${encodeURIComponent(id)}/values/${encodeURIComponent(range)}${query({
      valueRenderOption: args.value_render_option,
      majorDimension: args.major_dimension,
    })}`,
  );
  return { value_range: valueRange };
}

/**
 * Read several A1 ranges from one spreadsheet in one request. Returns `valueRanges` in the order
 * requested; a range with no data comes back without `values`.
 */
export async function batch_read(
  args: { spreadsheet_id: string; ranges: string[]; value_render_option?: string },
  ctx: Ctx,
): Promise<{ spreadsheetId: string; valueRanges: ValueRange[] }> {
  if (args.ranges.length === 0) throw new Error("Provide at least one range");
  const id = spreadsheetId(args.spreadsheet_id);
  // `ranges` repeats: a tab title may itself contain a comma.
  const ranges = args.ranges.map((range) => `ranges=${encodeURIComponent(range)}`).join("&");
  const render = query({ valueRenderOption: args.value_render_option });
  const result = await sheetsRequest(
    ctx,
    `/spreadsheets/${encodeURIComponent(id)}/values:batchGet?${ranges}${render ? `&${render.slice(1)}` : ""}`,
  );
  return { spreadsheetId: result.spreadsheetId ?? id, valueRanges: result.valueRanges ?? [] };
}

/**
 * Write `values` (one array per row) into one A1 range, growing it as needed.
 * `value_input_option` is `USER_ENTERED` (default: Sheets parses formulas, numbers, dates) or `RAW`
 * (stored verbatim). Returns the updated range and row/column/cell counts.
 */
export async function update(
  args: {
    spreadsheet_id: string;
    range: string;
    values: CellValue[][];
    value_input_option?: string;
    include_values_in_response?: boolean;
  },
  ctx: Ctx,
): Promise<{ result: UpdateValuesResponse }> {
  const id = spreadsheetId(args.spreadsheet_id);
  const result = await sheetsRequest(
    ctx,
    `/spreadsheets/${encodeURIComponent(id)}/values/${encodeURIComponent(args.range)}${query({
      valueInputOption: args.value_input_option ?? "USER_ENTERED",
      includeValuesInResponse: args.include_values_in_response === true,
    })}`,
    { method: "PUT", body: { values: args.values } },
  );
  return { result };
}

/**
 * Write several ranges of one spreadsheet in one request. `data` is a list of
 * `{ range, values }` with `values` as rows of cells. `value_input_option` is `USER_ENTERED`
 * (default) or `RAW`. Returns the total counts and one response per range.
 */
export async function batch_update(
  args: {
    spreadsheet_id: string;
    data: { range: string; values: CellValue[][] }[];
    value_input_option?: string;
  },
  ctx: Ctx,
): Promise<{ result: BatchUpdateValuesResponse }> {
  const id = spreadsheetId(args.spreadsheet_id);
  const result = await sheetsRequest(
    ctx,
    `/spreadsheets/${encodeURIComponent(id)}/values:batchUpdate`,
    {
      body: {
        valueInputOption: args.value_input_option ?? "USER_ENTERED",
        data: args.data.map((entry) => ({ range: entry.range, values: entry.values })),
      },
    },
  );
  return { result };
}

/**
 * Write a header row and table body into a named tab, starting at `start_cell` (default `A1`).
 * Rows are either objects keyed by header (missing headers become empty cells) or positional arrays
 * truncated/padded to `headers`. `value_input_option` is `USER_ENTERED` (default) or `RAW`. The tab
 * is addressed by title, so a title containing spaces or quotes is fine. Returns the update result
 * plus `sheet_title`, `headers`, `row_count` and `header_count`.
 */
export async function write_table(
  args: {
    spreadsheet_id: string;
    sheet_title: string;
    headers: string[];
    rows: TableRow[];
    start_cell?: string;
    value_input_option?: string;
  },
  ctx: Ctx,
): Promise<{
  result: UpdateValuesResponse;
  sheet_title: string;
  headers: string[];
  row_count: number;
  header_count: number;
}> {
  if (args.headers.length === 0) throw new Error("headers must not be empty");
  const headerCount = args.headers.length;
  const tableRows: CellValue[][] = args.rows.map((row) => {
    if (Array.isArray(row)) {
      const cells: CellValue[] = [];
      for (let index = 0; index < headerCount; index += 1) {
        const value = index < row.length ? row[index] : "";
        cells.push(value === undefined || value === null ? "" : value);
      }
      return cells;
    }
    return args.headers.map((header) => {
      const value = row[header];
      return value === undefined || value === null ? "" : value;
    });
  });
  const { result } = await update(
    {
      spreadsheet_id: args.spreadsheet_id,
      range: `${quoteSheetTitle(args.sheet_title)}!${args.start_cell ?? "A1"}`,
      values: [args.headers, ...tableRows],
      value_input_option: args.value_input_option,
    },
    ctx,
  );
  return {
    result,
    sheet_title: args.sheet_title,
    headers: args.headers,
    row_count: tableRows.length,
    header_count: headerCount,
  };
}
