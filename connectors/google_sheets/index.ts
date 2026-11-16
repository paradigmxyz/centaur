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

/** A rectangle of one tab, zero-based and end-exclusive; a missing bound is unbounded. */
export interface GridRange {
  sheetId: number;
  startRowIndex?: number;
  endRowIndex?: number;
  startColumnIndex?: number;
  endColumnIndex?: number;
}

/** The outcome of a spreadsheet `batchUpdate`: one reply per request, in order. */
export interface BatchUpdateSpreadsheetResponse {
  spreadsheetId?: string;
  replies?: unknown[];
  [field: string]: unknown;
}

/** Cell formatting for `format_range`. Options left out are not changed. */
export interface CellFormatOptions {
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strikethrough?: boolean;
  /** Points. */
  font_size?: number;
  /** Such as `Roboto Mono`. */
  font_family?: string;
  /** Hex, `#RGB` or `#RRGGBB`. */
  text_color?: string;
  /** Cell fill, hex `#RGB` or `#RRGGBB`. */
  background_color?: string;
  /** `LEFT`, `CENTER` or `RIGHT`. */
  horizontal_alignment?: string;
  /** `TOP`, `MIDDLE` or `BOTTOM`. */
  vertical_alignment?: string;
  /** `OVERFLOW_CELL`, `LEGACY_WRAP`, `CLIP` or `WRAP`. */
  wrap_strategy?: string;
  /** `TEXT`, `NUMBER`, `PERCENT`, `CURRENCY`, `DATE`, `TIME`, `DATE_TIME` or `SCIENTIFIC`. */
  number_format_type?: string;
  /** A Sheets number format pattern such as `#,##0.00`; type defaults to `NUMBER`. */
  number_format_pattern?: string;
}

const HORIZONTAL_ALIGNMENTS = ["LEFT", "CENTER", "RIGHT"];
const VERTICAL_ALIGNMENTS = ["TOP", "MIDDLE", "BOTTOM"];
const WRAP_STRATEGIES = ["OVERFLOW_CELL", "LEGACY_WRAP", "CLIP", "WRAP"];
const NUMBER_FORMAT_TYPES = [
  "TEXT",
  "NUMBER",
  "PERCENT",
  "CURRENCY",
  "DATE",
  "TIME",
  "DATE_TIME",
  "SCIENTIFIC",
];

interface TabRef {
  sheetId: number;
  title: string;
}

/** Tabs in display order. The API omits `sheetId` and `index` when they are 0. */
async function listTabs(ctx: Ctx, id: string): Promise<TabRef[]> {
  const result = await sheetsRequest(
    ctx,
    `/spreadsheets/${encodeURIComponent(id)}${query({
      fields: "sheets.properties(sheetId,title,index)",
    })}`,
  );
  const props: SheetProperties[] = (result.sheets ?? []).map((sheet: Sheet) => sheet.properties ?? {});
  props.sort((a, b) => (a.index ?? 0) - (b.index ?? 0));
  return props.map((p) => ({ sheetId: p.sheetId ?? 0, title: p.title ?? "" }));
}

function resolveSheetId(tabs: TabRef[], title: string | undefined): number {
  if (tabs.length === 0) throw new Error("Spreadsheet has no tabs");
  if (title === undefined) return tabs[0].sheetId;
  const tab = tabs.find((t) => t.title === title);
  if (!tab) {
    const titles = tabs.map((t) => JSON.stringify(t.title)).join(", ");
    throw new Error(`No tab titled ${JSON.stringify(title)} (tabs: ${titles})`);
  }
  return tab.sheetId;
}

/** Split `'Tab'!A1:B2` into its tab title and cell part. */
function splitSheetRange(notation: string): { title?: string; cells?: string } {
  const value = notation.trim();
  if (value.startsWith("'")) {
    let title = "";
    let index = 1;
    for (;;) {
      if (index >= value.length) throw new Error(`Unterminated quoted tab title in range: ${notation}`);
      const char = value[index];
      if (char === "'") {
        if (value[index + 1] === "'") {
          title += "'";
          index += 2;
          continue;
        }
        break;
      }
      title += char;
      index += 1;
    }
    const rest = value.slice(index + 1);
    if (rest === "") return { title };
    if (!rest.startsWith("!")) throw new Error(`Invalid A1 range: ${notation}`);
    return { title, cells: rest.slice(1) || undefined };
  }
  const bang = value.lastIndexOf("!");
  if (bang >= 0) return { title: value.slice(0, bang), cells: value.slice(bang + 1) || undefined };
  return { cells: value || undefined };
}

function columnIndex(letters: string): number {
  let index = 0;
  for (const letter of letters.toUpperCase()) index = index * 26 + (letter.charCodeAt(0) - 64);
  return index - 1;
}

/** Parse `A1`, `A1:C3`, `A:C`, `2:5` or `A2:C` into GridRange bounds; `undefined` if not A1. */
function parseA1Cells(cells: string): Omit<GridRange, "sheetId"> | undefined {
  const parts = cells.split(":");
  if (parts.length > 2) return undefined;
  const parsed: { column?: number; row?: number }[] = [];
  for (const part of parts) {
    const match = /^\$?([A-Za-z]*)\$?(\d*)$/.exec(part.trim());
    if (!match || (!match[1] && !match[2])) return undefined;
    const row = match[2] ? Number(match[2]) - 1 : undefined;
    if (row !== undefined && row < 0) return undefined;
    parsed.push({ column: match[1] ? columnIndex(match[1]) : undefined, row });
  }
  const start = parsed[0];
  const end = parsed[parsed.length - 1];
  if (parsed.length === 1 && (start.column === undefined || start.row === undefined)) return undefined;
  const bounds: Omit<GridRange, "sheetId"> = {};
  type Bound = number | undefined;
  const span = (a: Bound, b: Bound): [Bound, Bound] => {
    if (a !== undefined && b !== undefined) return [Math.min(a, b), Math.max(a, b) + 1];
    return [a, b === undefined ? undefined : b + 1];
  };
  const [startColumn, endColumn] = span(start.column, end.column);
  const [startRow, endRow] = span(start.row, end.row);
  if (startRow !== undefined) bounds.startRowIndex = startRow;
  if (endRow !== undefined) bounds.endRowIndex = endRow;
  if (startColumn !== undefined) bounds.startColumnIndex = startColumn;
  if (endColumn !== undefined) bounds.endColumnIndex = endColumn;
  return bounds;
}

/** An A1 range without a tab is on the first tab, unless the whole string is a tab title. */
function gridRange(tabs: TabRef[], notation: string): GridRange {
  let { title, cells } = splitSheetRange(notation);
  if (title === undefined && cells !== undefined && tabs.some((t) => t.title === cells)) {
    title = cells;
    cells = undefined;
  }
  const range: GridRange = { sheetId: resolveSheetId(tabs, title) };
  if (cells === undefined) return range;
  const bounds = parseA1Cells(cells);
  if (!bounds) throw new Error(`Invalid A1 range: ${notation}`);
  return { ...range, ...bounds };
}

function parseColor(color: string): { red: number; green: number; blue: number } {
  let hex = color.trim().replace(/^#/, "");
  if (hex.length === 3) hex = [...hex].map((c) => c + c).join("");
  if (!/^[0-9a-fA-F]{6}$/.test(hex)) {
    throw new Error(`Color must be hex like "#1a73e8", got ${JSON.stringify(color)}`);
  }
  return {
    red: parseInt(hex.slice(0, 2), 16) / 255,
    green: parseInt(hex.slice(2, 4), 16) / 255,
    blue: parseInt(hex.slice(4, 6), 16) / 255,
  };
}

function choice(name: string, value: string, allowed: string[]): string {
  const upper = value.toUpperCase();
  if (!allowed.includes(upper)) {
    throw new Error(`${name} must be one of ${allowed.join(", ")}, got ${JSON.stringify(value)}`);
  }
  return upper;
}

/** A Sheets CellFormat plus its `userEnteredFormat.*` field mask. */
function cellFormat(options: CellFormatOptions): { format: Record<string, unknown>; fields: string[] } {
  const format: Record<string, unknown> = {};
  const fields: string[] = [];
  const text: Record<string, unknown> = {};
  if (options.bold !== undefined) text.bold = options.bold;
  if (options.italic !== undefined) text.italic = options.italic;
  if (options.underline !== undefined) text.underline = options.underline;
  if (options.strikethrough !== undefined) text.strikethrough = options.strikethrough;
  if (options.font_family !== undefined) text.fontFamily = options.font_family;
  if (options.font_size !== undefined) {
    if (!(options.font_size > 0)) throw new Error("font_size must be positive");
    text.fontSize = options.font_size;
  }
  if (options.text_color !== undefined) {
    text.foregroundColorStyle = { rgbColor: parseColor(options.text_color) };
  }
  if (Object.keys(text).length > 0) {
    format.textFormat = text;
    for (const key of Object.keys(text)) fields.push(`userEnteredFormat.textFormat.${key}`);
  }
  if (options.background_color !== undefined) {
    format.backgroundColorStyle = { rgbColor: parseColor(options.background_color) };
    fields.push("userEnteredFormat.backgroundColorStyle");
  }
  if (options.horizontal_alignment !== undefined) {
    format.horizontalAlignment = choice(
      "horizontal_alignment",
      options.horizontal_alignment,
      HORIZONTAL_ALIGNMENTS,
    );
    fields.push("userEnteredFormat.horizontalAlignment");
  }
  if (options.vertical_alignment !== undefined) {
    format.verticalAlignment = choice(
      "vertical_alignment",
      options.vertical_alignment,
      VERTICAL_ALIGNMENTS,
    );
    fields.push("userEnteredFormat.verticalAlignment");
  }
  if (options.wrap_strategy !== undefined) {
    format.wrapStrategy = choice("wrap_strategy", options.wrap_strategy, WRAP_STRATEGIES);
    fields.push("userEnteredFormat.wrapStrategy");
  }
  if (options.number_format_type !== undefined || options.number_format_pattern !== undefined) {
    const numberFormat: Record<string, unknown> = {
      type: choice("number_format_type", options.number_format_type ?? "NUMBER", NUMBER_FORMAT_TYPES),
    };
    if (options.number_format_pattern !== undefined) {
      numberFormat.pattern = options.number_format_pattern;
    }
    format.numberFormat = numberFormat;
    fields.push("userEnteredFormat.numberFormat");
  }
  return { format, fields };
}

/**
 * Format the cells of one A1 range: bold/italic/underline/strikethrough, font size and family,
 * text and background colors (hex), alignment, wrapping and number format. Only options given are
 * changed. `range` is `'Tab'!A1:D1`, `A:A`, `2:2`, `A2:C` or a bare tab title for the whole tab;
 * without a tab it is the first tab. Returns the batchUpdate response and the resolved grid range.
 */
export async function format_range(
  args: { spreadsheet_id: string; range: string } & CellFormatOptions,
  ctx: Ctx,
): Promise<{ result: BatchUpdateSpreadsheetResponse; grid_range: GridRange; fields: string[] }> {
  const { format, fields } = cellFormat(args);
  if (fields.length === 0) throw new Error("Provide at least one format option");
  const id = spreadsheetId(args.spreadsheet_id);
  const range = gridRange(await listTabs(ctx, id), args.range);
  const result = await sheetsRequest(ctx, `/spreadsheets/${encodeURIComponent(id)}:batchUpdate`, {
    body: {
      requests: [
        { repeatCell: { range, cell: { userEnteredFormat: format }, fields: fields.join(",") } },
      ],
    },
  });
  return { result, grid_range: range, fields };
}

/**
 * Freeze the top `rows` and/or left `columns` of a tab (`sheet_title`, default the first tab);
 * 0 unfreezes. Returns the batchUpdate response and the tab's `sheetId`.
 */
export async function freeze(
  args: { spreadsheet_id: string; sheet_title?: string; rows?: number; columns?: number },
  ctx: Ctx,
): Promise<{ result: BatchUpdateSpreadsheetResponse; sheet_id: number }> {
  const gridProperties: Record<string, number> = {};
  for (const [key, value] of [
    ["frozenRowCount", args.rows],
    ["frozenColumnCount", args.columns],
  ] as const) {
    if (value === undefined) continue;
    if (!Number.isInteger(value) || value < 0) {
      throw new Error("rows and columns must be non-negative integers");
    }
    gridProperties[key] = value;
  }
  const keys = Object.keys(gridProperties);
  if (keys.length === 0) throw new Error("Provide rows and/or columns");
  const id = spreadsheetId(args.spreadsheet_id);
  const sheetId = resolveSheetId(await listTabs(ctx, id), args.sheet_title);
  const result = await sheetsRequest(ctx, `/spreadsheets/${encodeURIComponent(id)}:batchUpdate`, {
    body: {
      requests: [
        {
          updateSheetProperties: {
            properties: { sheetId, gridProperties },
            fields: keys.map((key) => `gridProperties.${key}`).join(","),
          },
        },
      ],
    },
  });
  return { result, sheet_id: sheetId };
}

/**
 * Send raw Sheets `requests` (repeatCell, updateBorders, mergeCells, updateDimensionProperties,
 * autoResizeDimensions, addConditionalFormatRule, addBanding, updateSheetProperties, ... see the
 * Sheets spreadsheets.batchUpdate reference) to one spreadsheet, for anything `format_range` and
 * `freeze` do not cover. Ranges are GridRanges keyed by numeric `sheetId` (from `get_spreadsheet`;
 * an omitted `sheetId` is 0). Requests apply in order, atomically. Returns one reply per request.
 */
export async function spreadsheet_batch_update(
  args: { spreadsheet_id: string; requests: unknown[] },
  ctx: Ctx,
): Promise<{ result: BatchUpdateSpreadsheetResponse }> {
  if (args.requests.length === 0) throw new Error("Provide at least one request");
  const id = spreadsheetId(args.spreadsheet_id);
  const result = await sheetsRequest(ctx, `/spreadsheets/${encodeURIComponent(id)}:batchUpdate`, {
    body: { requests: args.requests },
  });
  return { result };
}
