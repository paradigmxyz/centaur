import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Figma REST API v1 with a personal access token. Responses are Figma's own
// objects; `design_system` additionally walks the document tree to summarize
// colors, type styles, components, variables and frames.

const DEFAULT_BASE = "https://api.figma.com/v1";

/** Who the access token belongs to. */
export interface FigmaUser {
  id: string;
  handle: string;
  img_url?: string;
  email?: string;
  [field: string]: unknown;
}

/** RGBA in 0..1, as Figma stores paint colors. */
export interface Color {
  r: number;
  g: number;
  b: number;
  a?: number;
  [field: string]: unknown;
}

/** One paint of a node (`SOLID`, `GRADIENT_LINEAR`, `IMAGE`, ...). */
export interface Paint {
  type: string;
  visible?: boolean;
  opacity?: number;
  color?: Color;
  [field: string]: unknown;
}

/** A node's text style overrides. */
export interface TypeStyle {
  fontFamily?: string;
  fontPostScriptName?: string;
  fontSize?: number;
  fontWeight?: number;
  lineHeightPx?: number;
  lineHeightPercent?: number;
  letterSpacing?: number;
  textAlignHorizontal?: string;
  [field: string]: unknown;
}

/** A node's references to shared library styles, by style ID. */
export interface NodeStyleRefs {
  fill?: string;
  text?: string;
  effect?: string;
  grid?: string;
  [field: string]: unknown;
}

/** A node in a Figma document tree; `children` nests arbitrarily deep. */
export interface FigmaNode {
  id: string;
  name: string;
  /** `DOCUMENT`, `CANVAS`, `FRAME`, `GROUP`, `COMPONENT`, `COMPONENT_SET`, `TEXT`, `RECTANGLE`, ... */
  type: string;
  children?: FigmaNode[];
  fills?: Paint[];
  strokes?: Paint[];
  style?: TypeStyle;
  styles?: NodeStyleRefs;
  description?: string;
  visible?: boolean;
  absoluteBoundingBox?: { x: number; y: number; width: number; height: number };
  [field: string]: unknown;
}

/** A published style. In file responses the type is `styleType`; in `/styles` it is `style_type`. */
export interface Style {
  key: string;
  name: string;
  description?: string;
  styleType?: string;
  style_type?: string;
  node_id?: string;
  file_key?: string;
  [field: string]: unknown;
}

/** A component or component set definition. */
export interface Component {
  key: string;
  name: string;
  description?: string;
  node_id?: string;
  file_key?: string;
  containing_frame?: { name?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** One mode (theme, breakpoint, ...) of a variable collection. */
export interface VariableMode {
  modeId: string;
  name: string;
  [field: string]: unknown;
}

/** A local variable collection. */
export interface VariableCollection {
  id: string;
  name: string;
  modes?: VariableMode[];
  defaultModeId?: string;
  [field: string]: unknown;
}

/** A local design variable; `valuesByMode` maps mode IDs to values. */
export interface Variable {
  id: string;
  name: string;
  /** `BOOLEAN`, `FLOAT`, `STRING`, `COLOR`. */
  resolvedType?: string;
  valuesByMode?: Record<string, unknown>;
  variableCollectionId?: string;
  [field: string]: unknown;
}

/** `GET /files/:key/variables/local`: Figma wraps the maps in `meta`. */
export interface LocalVariables {
  status?: number;
  error?: boolean;
  meta?: {
    variables?: Record<string, Variable>;
    variableCollections?: Record<string, VariableCollection>;
    [field: string]: unknown;
  };
  variables?: Record<string, Variable>;
  variableCollections?: Record<string, VariableCollection>;
  [field: string]: unknown;
}

/** A Figma file document (the `/files/:key` response body). */
export interface FigmaFile {
  name: string;
  role?: string;
  lastModified?: string;
  version?: string;
  editorType?: string;
  thumbnailUrl?: string;
  schemaVersion?: number;
  document: FigmaNode;
  components?: Record<string, Component>;
  componentSets?: Record<string, Component>;
  styles?: Record<string, Style>;
  [field: string]: unknown;
}

/** `GET /files/:key/nodes`: `nodes` is keyed by node ID. */
export interface NodesResponse {
  nodes: Record<string, { document: FigmaNode; components?: Record<string, Component>; styles?: Record<string, Style>; [field: string]: unknown }>;
  [field: string]: unknown;
}

/** `GET /files/:key/styles` or `/components`: Figma wraps the list in `meta`. */
export interface MetaList<T> {
  status?: number;
  error?: boolean;
  meta?: { styles?: T[]; components?: T[]; [field: string]: unknown };
  [field: string]: unknown;
}

/** A color found while walking the document: either a named style or a fill value. */
export interface ColorEntry {
  /** Hex (`#rrggbb`) or `rgba(r, g, b, a)` for fill-derived entries. */
  value?: string;
  /** Node or layer name the fill came from. */
  source?: string;
  /** Style ID for style-derived entries. */
  id?: string;
  name?: string;
  description?: string;
}

/** A text style found while walking the document, or a published TEXT style. */
export interface TextStyleEntry {
  source?: string;
  fontFamily?: string;
  fontSize?: number;
  fontWeight?: number;
  /** `lineHeightPx`. */
  lineHeight?: number;
  letterSpacing?: number;
  id?: string;
  name?: string;
  description?: string;
}

/** A component found in the document tree. */
export interface ComponentEntry {
  id: string;
  name: string;
  description?: string;
}

/** A local variable, flattened from `valuesByMode`. */
export interface VariableEntry {
  id: string;
  name: string;
  /** `resolvedType`. */
  type?: string;
  values?: Record<string, unknown>;
}

/** An artboard near the top of the document (depth 0–2) with its background color. */
export interface FrameEntry {
  id: string;
  name: string;
  width?: number;
  height?: number;
  /** Background fill as `#rrggbb` or `rgba(...)`. */
  background?: string;
}

/** A published EFFECT or GRID style. */
export interface StyleRefEntry {
  id: string;
  name: string;
  description?: string;
}

/** The design-system summary `design_system` extracts from one file. */
export interface DesignSystem {
  file_name: string;
  colors: ColorEntry[];
  text_styles: TextStyleEntry[];
  components: ComponentEntry[];
  variables: VariableEntry[];
  frames: FrameEntry[];
  /** Published EFFECT styles. */
  effects: StyleRefEntry[];
  /** Published GRID styles. */
  grids: StyleRefEntry[];
}

/**
 * A parsed Figma link: `file_key` for the API, `node_id` with a colon (`1:2`) when the link
 * pointed at a node (`?node-id=1-2` is normalized).
 */
export interface ParsedUrl {
  file_key: string;
  node_id: string | null;
}

function base(ctx: Ctx): string {
  return (ctx.env.FIGMA_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

/** Retries a rate-limited (429) request up to `retries` times, honoring `Retry-After`. */
async function request(
  ctx: Ctx,
  path: string,
  label: string,
  retries: number = 2,
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the token is injected on the wire; locally it comes from the env.
  if (ctx.env.FIGMA_ACCESS_TOKEN) headers["X-Figma-Token"] = ctx.env.FIGMA_ACCESS_TOKEN;
  for (let attempt = 0; ; attempt += 1) {
    const res = await ctx.fetch(`${base(ctx)}${path}`, { headers });
    if (res.status === 429 && attempt < retries) {
      const seconds = Number(res.headers["retry-after"] || "1");
      await ctx.sleep(Math.min(Number.isFinite(seconds) ? seconds : 1, 10) * 1000);
      continue;
    }
    return readJson(res, label);
  }
}

function parseFigmaUrl(url: string): ParsedUrl {
  const match = /figma\.com\/(?:file|design|proto|board)\/([a-zA-Z0-9]+)/.exec(url);
  if (!match) throw new Error(`Invalid Figma URL: ${url}`);
  const nodeMatch = /node-id=([0-9:-]+)/.exec(url);
  return {
    file_key: match[1],
    node_id: nodeMatch ? nodeMatch[1].replace(/-/g, ":") : null,
  };
}

/** Hex or rgba() for a SOLID paint, else undefined. */
function fillColor(fill: Paint): string | undefined {
  if (fill?.type !== "SOLID" || !fill.color) return undefined;
  const r = Math.floor((fill.color.r ?? 0) * 255);
  const g = Math.floor((fill.color.g ?? 0) * 255);
  const b = Math.floor((fill.color.b ?? 0) * 255);
  const a = fill.color.a ?? 1;
  if (a < 1) return `rgba(${r}, ${g}, ${b}, ${a.toFixed(2)})`;
  const hex = (value: number): string => (value < 16 ? "0" : "") + value.toString(16);
  return `#${hex(r)}${hex(g)}${hex(b)}`;
}

function styleType(style: Style): string {
  return style.styleType || style.style_type || "";
}

function styleRef(style_id: string, style: Style): StyleRefEntry {
  return { id: style_id, name: style.name || "", description: style.description || "" };
}

function walk(node: FigmaNode, ds: DesignSystem, depth: number): void {
  const name = node.name || "";
  if (node.type === "COMPONENT") {
    ds.components.push({ id: node.id || "", name, description: node.description || "" });
  }
  if (node.type === "FRAME" && depth <= 2) {
    const fills = node.fills ?? [];
    const frame: FrameEntry = { id: node.id || "", name };
    if (node.absoluteBoundingBox?.width !== undefined) frame.width = node.absoluteBoundingBox.width;
    if (node.absoluteBoundingBox?.height !== undefined) frame.height = node.absoluteBoundingBox.height;
    const background = fills.length ? fillColor(fills[0]) : undefined;
    if (background) frame.background = background;
    ds.frames.push(frame);
  }
  for (const fill of node.fills ?? []) {
    const value = fillColor(fill);
    if (value && !ds.colors.some((color) => color.value === value)) {
      ds.colors.push({ value, source: name });
    }
  }
  if (node.type === "TEXT" && node.style) {
    const entry: TextStyleEntry = {
      source: name,
      fontFamily: node.style.fontFamily || "",
      fontSize: node.style.fontSize || 0,
      fontWeight: node.style.fontWeight || 400,
      letterSpacing: node.style.letterSpacing || 0,
    };
    if (node.style.lineHeightPx !== undefined) entry.lineHeight = node.style.lineHeightPx;
    const key = `${entry.fontFamily}-${entry.fontSize}-${entry.fontWeight}`;
    const seen = ds.text_styles.some(
      (text) => `${text.fontFamily}-${text.fontSize}-${text.fontWeight}` === key,
    );
    if (!seen) ds.text_styles.push(entry);
  }
  for (const child of node.children ?? []) walk(child, ds, depth + 1);
}

/**
 * The user (and email) the access token belongs to. Read-only check that the token works.
 */
export async function me(args: {}, ctx: Ctx): Promise<{ user: FigmaUser }> {
  return { user: await request(ctx, "/me", "Figma /me") };
}

/**
 * The full document tree of a file plus its published `components`, `componentSets` and
 * `styles` maps. `file_key` is the ID in a Figma URL (`figma.com/design/<file_key>/…`), or pass
 * `url` and it is parsed. `ids` limits the returned subtree to those node IDs, `depth` limits
 * how many levels below them are returned (1 is a cheap thumbnail-ish overview), `geometry`
 * includes absolute bounding boxes (needed for sizes and colors in context), and `version`
 * pins a file version. Large files overflow the 10 MiB response cap: use `ids`/`depth`.
 */
export async function get_file(
  args: {
    file_key?: string;
    url?: string;
    ids?: string[];
    depth?: number;
    geometry?: boolean;
    version?: string;
    branch_data?: boolean;
  },
  ctx: Ctx,
): Promise<{ file: FigmaFile }> {
  const key = fileKey(args);
  const path = `/files/${encodeURIComponent(key)}${query({
    ids: args.ids?.length ? args.ids.join(",") : undefined,
    depth: args.depth,
    geometry: args.geometry ? "paths" : undefined,
    version: args.version,
    branch_data: args.branch_data ? "true" : undefined,
  })}`;
  return { file: await request(ctx, path, "Figma /files") };
}

/**
 * One or more nodes by ID from a file, as `{ nodes: { [nodeId]: { document, components,
 * styles } } }`. Node IDs use colons (`1:2`); a hyphenated link value is normalized. Use it
 * instead of `get_file` to read a single frame from a big file.
 */
export async function get_node(
  args: { file_key?: string; url?: string; ids: string[]; depth?: number; geometry?: boolean; version?: string },
  ctx: Ctx,
): Promise<NodesResponse> {
  const key = fileKey(args);
  const ids = (args.ids ?? []).map((id) => id.replace(/-/g, ":"));
  if (!ids.length) throw new Error("ids is required (node IDs such as 1:2)");
  const path = `/files/${encodeURIComponent(key)}/nodes${query({
    ids: ids.join(","),
    depth: args.depth,
    geometry: args.geometry ? "paths" : undefined,
    version: args.version,
  })}`;
  return request(ctx, path, "Figma /files/nodes");
}

/** Published (library) styles of a file: FILL, TEXT, EFFECT and GRID styles with keys and names. */
export async function get_file_styles(
  args: { file_key?: string; url?: string },
  ctx: Ctx,
): Promise<MetaList<Style>> {
  const key = fileKey(args);
  return request(ctx, `/files/${encodeURIComponent(key)}/styles`, "Figma /files/styles");
}

/** Published (library) components of a file, with keys, names and descriptions. */
export async function get_file_components(
  args: { file_key?: string; url?: string },
  ctx: Ctx,
): Promise<MetaList<Component>> {
  const key = fileKey(args);
  return request(ctx, `/files/${encodeURIComponent(key)}/components`, "Figma /files/components");
}

/**
 * Local variables and variable collections of a file. Values live in
 * `meta.variables[id].valuesByMode[modeId]` and `resolvedType` is BOOLEAN, FLOAT, STRING or
 * COLOR. Needs a token with the `file_variables:read` scope; Figma answers 403 otherwise.
 */
export async function get_file_variables(
  args: { file_key?: string; url?: string },
  ctx: Ctx,
): Promise<LocalVariables> {
  const key = fileKey(args);
  return request(ctx, `/files/${encodeURIComponent(key)}/variables/local`, "Figma /files/variables/local");
}

/**
 * Summarize a file's design system from a file or frame URL: colors (published FILL styles and
 * distinct solid fills with their layer names), text styles (deduped by family/size/weight),
 * components, local variables, frames up to depth 2 with their background colors, and published
 * EFFECT and GRID styles. `variables` is empty when the token lacks `file_variables:read`.
 */
export async function design_system(
  args: { url?: string; file_key?: string },
  ctx: Ctx,
): Promise<{ design_system: DesignSystem }> {
  const key = fileKey(args);
  const file: FigmaFile = await request(ctx, `/files/${encodeURIComponent(key)}`, "Figma /files");
  const ds: DesignSystem = {
    file_name: file.name || "Unknown",
    colors: [],
    text_styles: [],
    components: [],
    variables: [],
    frames: [],
    effects: [],
    grids: [],
  };
  const styles = file.styles ?? {};
  for (const style_id of Object.keys(styles)) {
    const style = styles[style_id];
    const type = styleType(style);
    if (type === "FILL") {
      ds.colors.push({ id: style_id, name: style.name || "", description: style.description || "" });
    } else if (type === "TEXT") {
      ds.text_styles.push({ id: style_id, name: style.name || "", description: style.description || "" });
    } else if (type === "EFFECT") {
      ds.effects.push(styleRef(style_id, style));
    } else if (type === "GRID") {
      ds.grids.push(styleRef(style_id, style));
    }
  }
  if (file.document) walk(file.document, ds, 0);

  try {
    const local: LocalVariables = await get_file_variables({ file_key: key }, ctx);
    const variables = local.meta?.variables ?? local.variables ?? {};
    for (const variable_id of Object.keys(variables)) {
      const variable = variables[variable_id];
      const entry: VariableEntry = { id: variable_id, name: variable.name || "" };
      if (variable.resolvedType) entry.type = variable.resolvedType;
      if (variable.valuesByMode) entry.values = variable.valuesByMode;
      ds.variables.push(entry);
    }
  } catch {
    // No file_variables:read scope, or variables are unavailable for this file.
  }
  return { design_system: ds };
}

/**
 * Parse a Figma link into the `file_key` and optional `node_id` the API wants
 * (`figma.com/file/…`, `/design/…`, `/proto/…` and `/board/…` links work; a hyphenated
 * `node-id=1-2` becomes `1:2`).
 */
export async function parse_url(args: { url: string }, ctx: Ctx): Promise<ParsedUrl> {
  return parseFigmaUrl(args.url);
}

function fileKey(args: { file_key?: string; url?: string }): string {
  if (args.file_key && args.file_key.trim()) return args.file_key.trim();
  if (args.url && args.url.trim()) return parseFigmaUrl(args.url).file_key;
  throw new Error("file_key or url is required");
}
