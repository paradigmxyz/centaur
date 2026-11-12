// Form values for the secret create/edit pages and their mapping to and from
// the per-kind /api/v1 payloads. Pure functions so they can be unit tested.
//
// Inline (control_plane) values are write-only: the API never returns them,
// the form never prefills them, and on edit a blank inline value is omitted so
// the server keeps the stored one (PATCH semantics, docs/API.md#secret-sources).
import { recordToRows, rowsToRecord, type KeyValueRow } from "@/components/form/key-value-rows";

import type { FormKind, KindRecord, SourcePayload } from "./api";

/** SecretSource::SOURCE_TYPES with the form's labels. */
export const SOURCE_TYPE_OPTIONS = [
  { value: "env", label: "Env Var" },
  { value: "aws_sm", label: "AWS Secrets Manager" },
  { value: "aws_ssm", label: "AWS SSM Parameter Store" },
  { value: "1password", label: "1Password" },
  { value: "1password_connect", label: "1Password Connect" },
  { value: "control_plane", label: "Inline Value" },
  { value: "token_broker", label: "Token Broker" },
] as const;

/** SecretKinds::SOURCE_REF_KEYS: the config key holding a source's reference. */
export const SOURCE_REF_KEYS: Record<string, string> = {
  env: "var",
  aws_sm: "secret_id",
  aws_ssm: "name",
  "1password": "secret_ref",
  "1password_connect": "secret_ref",
  token_broker: "credential_id",
};

export const REFERENCE_LABELS: Record<string, string> = {
  env: "Environment variable",
  aws_sm: "Secret ID",
  aws_ssm: "Parameter name",
  "1password": "Secret reference",
  "1password_connect": "Secret reference",
  token_broker: "Credential ID",
};

export const AWS_SOURCE_TYPES = ["aws_sm", "aws_ssm"];

/** RequestRule::HTTP_METHODS + the "*" wildcard (shown as ALL). */
export const HTTP_METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "CONNECT"] as const;
export const METHOD_WILDCARD = "*";

export const SETTING_KINDS = [
  { value: "literal", label: "Literal" },
  { value: "principal_label", label: "Principal label" },
  { value: "principal_field", label: "Principal field" },
  { value: "proxy_label", label: "Proxy label" },
] as const;

export const STATIC_PROFILES = [
  { value: "custom", label: "Custom" },
  { value: "github_token", label: "GitHub token" },
] as const;

export const GCP_ID_TOKEN_HEADERS = [
  { value: "authorization", label: "Authorization" },
  { value: "x-serverless-authorization", label: "X-Serverless-Authorization" },
] as const;

export interface SourceValues {
  source_type: string;
  reference: string;
  /** Inline value; never prefilled. */
  secret: string;
  region: string;
  json_key: string;
}

export interface RuleValues {
  host: string;
  cidr: string;
  http_methods: string[];
  paths: string[];
}

export interface SettingValues {
  name: string;
  kind: string;
  value: string;
}

export interface SecretFormValues {
  foreign_id: string;
  name: string;
  description: string;
  labels: KeyValueRow[];
  source: SourceValues;
  rules: RuleValues[];
  // static
  profile: string;
  mode: "inject" | "replace";
  inject_header: string;
  query_param: string;
  formatter: string;
  proxy_value: string;
  match_headers: string;
  match_body: boolean;
  match_path: boolean;
  match_query: boolean;
  require: boolean;
  // pg_dsn
  database: string;
  role: string;
  settings: SettingValues[];
  // gcp_auth
  credential_mode: "keyfile" | "workload_identity";
  subject: string;
  scopes: string;
  // gcp_id_token
  audience: string;
  id_token_header: string;
}

const str = (value: unknown) => (typeof value === "string" ? value : value == null ? "" : String(value));
const blankToNull = (value: string) => (value.trim() === "" ? null : value.trim());

/** Splits on whitespace and commas (RuleParams#split_terms). */
export function splitTerms(value: string): string[] {
  return value
    .split(/[\s,]+/)
    .map((term) => term.trim())
    .filter(Boolean);
}

function sourceValues(source: SourcePayload | null | undefined): SourceValues {
  const type = source?.source_type || "env";
  const config = source?.config ?? {};
  const refKey = SOURCE_REF_KEYS[type];
  return {
    source_type: type,
    reference: refKey ? str(config[refKey]) : "",
    secret: "",
    region: str(config.region),
    json_key: str(config.json_key),
  };
}

/** The stored source of a record for its kind (source / dsn / keyfile). */
export function storedSource(kind: FormKind, record: KindRecord | undefined): SourcePayload | null {
  if (!record) return null;
  if (kind === "static") return record.source ?? null;
  if (kind === "pg_dsn") return record.dsn ?? null;
  return record.keyfile ?? null;
}

/** Form defaults for a new secret (record undefined) or an existing one. */
export function toFormValues(kind: FormKind, record?: KindRecord): SecretFormValues {
  const inject = record?.inject_config ?? {};
  const replace = record?.replace_config ?? {};
  return {
    foreign_id: str(record?.foreign_id),
    name: str(record?.name),
    description: str(record?.description),
    labels: recordToRows(record?.labels),
    source: sourceValues(storedSource(kind, record)),
    rules: (record?.rules ?? []).map((rule) => ({
      host: str(rule.host),
      cidr: str(rule.cidr),
      http_methods: [...(rule.http_methods ?? [])],
      paths: [...(rule.paths ?? [])],
    })),
    profile: record?.kind || "custom",
    mode: record?.replace_config ? "replace" : "inject",
    inject_header: str(inject.header),
    query_param: str(inject.query_param),
    formatter: str(inject.formatter),
    proxy_value: str(replace.proxy_value),
    match_headers: Array.isArray(replace.match_headers) ? replace.match_headers.map(str).join(", ") : "",
    match_body: replace.match_body === true,
    match_path: replace.match_path === true,
    match_query: replace.match_query === true,
    require: replace.require === true,
    database: str(record?.database),
    role: str(record?.role),
    settings: (record?.settings ?? []).map(settingRow),
    credential_mode: record?.credentials_provider ? "workload_identity" : "keyfile",
    subject: str(record?.subject),
    scopes: (record?.scopes ?? []).join("\n"),
    audience: str(record?.audience),
    id_token_header: record?.header || "authorization",
  };
}

/** pg_setting_row_attrs: a stored setting as { name, kind, value }. */
function settingRow(setting: { name: string; value?: string; value_from?: Record<string, string> }): SettingValues {
  const ref = setting.value_from;
  const kind = ref ? SETTING_KINDS.map((k) => k.value).find((k) => k !== "literal" && k in ref) : undefined;
  if (ref && kind) return { name: str(setting.name), kind, value: str(ref[kind]) };
  return { name: str(setting.name), kind: "literal", value: str(setting.value) };
}

/**
 * The source payload (BaseSecretsController#build_source). Config keys the
 * form doesn't edit (e.g. with_decryption, token_env) are kept when the backend
 * is unchanged. An inline value is sent only when typed.
 */
export function sourcePayload(values: SourceValues, stored: SourcePayload | null): Record<string, unknown> {
  const type = values.source_type;
  const refKey = SOURCE_REF_KEYS[type];
  const edited = new Set([refKey, "region", "json_key"]);
  const config: Record<string, unknown> =
    stored && stored.source_type === type
      ? Object.fromEntries(Object.entries(stored.config ?? {}).filter(([key]) => !edited.has(key)))
      : {};
  if (refKey && values.reference.trim()) config[refKey] = values.reference.trim();
  if (AWS_SOURCE_TYPES.includes(type) && values.region.trim()) config.region = values.region.trim();
  if (values.json_key.trim()) config.json_key = values.json_key.trim();

  const payload: Record<string, unknown> = { source_type: type, config };
  if (type === "control_plane" && values.secret !== "") payload.secret = values.secret;
  return payload;
}

/** Rules with all-blank rows dropped (RuleParams#assign_rules); position comes from order. */
export function rulesPayload(rules: RuleValues[]) {
  return rules
    .filter((rule) => rule.host.trim() || rule.cidr.trim() || rule.http_methods.length > 0 || rule.paths.length > 0)
    .map((rule) => ({
      host: blankToNull(rule.host),
      cidr: blankToNull(rule.cidr),
      http_methods: rule.http_methods.map((method) => method.toUpperCase()),
      paths: rule.paths,
    }));
}

/** Settings with blank-name rows dropped (PgDsnSecretsController#setting_rows). */
export function settingsPayload(settings: SettingValues[]) {
  return settings
    .filter((setting) => setting.name.trim())
    .map((setting) =>
      setting.kind === "literal"
        ? { name: setting.name.trim(), value: setting.value }
        : { name: setting.name.trim(), value_from: { [setting.kind]: setting.value.trim() } },
    );
}

/**
 * The foreign_id part of the body. A blank field is omitted (the server keeps
 * the stored identity) unless the user cleared an existing Foreign ID; only
 * then is it sent as an explicit null, which clears it on an update by OID
 * (docs/API.md#upsert-put--patch).
 */
export function foreignIdPayload(value: string, record?: KindRecord): { foreign_id?: string | null } {
  const trimmed = value.trim();
  if (trimmed) return { foreign_id: trimmed };
  return record?.foreign_id ? { foreign_id: null } : {};
}

/** The `data` body for POST/PATCH /api/v1/<kind>_secrets. */
export function toPayload(kind: FormKind, values: SecretFormValues, record?: KindRecord): Record<string, unknown> {
  const stored = storedSource(kind, record);
  const identity = {
    ...foreignIdPayload(values.foreign_id, record),
    name: blankToNull(values.name),
    description: blankToNull(values.description),
    labels: rowsToRecord(values.labels),
  };
  switch (kind) {
    case "static": {
      const body: Record<string, unknown> = {
        ...identity,
        kind: values.profile || "custom",
        source: sourcePayload(values.source, stored),
        rules: rulesPayload(values.rules),
      };
      if (values.mode === "replace") {
        const replace: Record<string, unknown> = { proxy_value: values.proxy_value };
        const headers = values.match_headers
          .split(",")
          .map((header) => header.trim())
          .filter(Boolean);
        if (headers.length > 0) replace.match_headers = headers;
        for (const flag of ["match_body", "match_path", "match_query", "require"] as const) {
          if (values[flag]) replace[flag] = true;
        }
        body.inject_config = null;
        body.replace_config = replace;
      } else {
        const inject: Record<string, unknown> = {};
        if (values.inject_header.trim()) inject.header = values.inject_header.trim();
        if (values.query_param.trim()) inject.query_param = values.query_param.trim();
        if (values.formatter) inject.formatter = values.formatter;
        body.inject_config = Object.keys(inject).length > 0 ? inject : null;
        body.replace_config = null;
      }
      return body;
    }
    case "pg_dsn":
      return {
        ...identity,
        database: blankToNull(values.database),
        role: blankToNull(values.role),
        settings: settingsPayload(values.settings),
        dsn: sourcePayload(values.source, stored),
      };
    case "gcp_auth": {
      const body: Record<string, unknown> = {
        ...identity,
        scopes: splitTerms(values.scopes),
        rules: rulesPayload(values.rules),
      };
      if (values.credential_mode === "workload_identity") {
        body.credentials_provider = { type: "workload_identity" };
        body.subject = null;
      } else {
        body.credentials_provider = null;
        body.subject = blankToNull(values.subject);
        body.keyfile = sourcePayload(values.source, stored);
      }
      return body;
    }
    case "gcp_id_token":
      return {
        ...identity,
        audience: blankToNull(values.audience),
        header: values.id_token_header === "authorization" ? null : values.id_token_header,
        keyfile: sourcePayload(values.source, stored),
        rules: rulesPayload(values.rules),
      };
  }
}

const NESTED_PATH = /^(?:(?:source|dsn|keyfile)\.|rules\[\d+\]\.)/;

/**
 * Reads the additive 422 form: nested errors arrive under path keys
 * ("source.config", "rules[0].cidr") and are repeated under the keys older
 * clients read ("config", "base", "rules": ["is invalid"]). Drops those
 * compatibility duplicates so each nested message appears once.
 */
export function normalizeDetails(details: Record<string, string[]>): Record<string, string[]> {
  const nested = new Set(
    Object.entries(details)
      .filter(([key]) => NESTED_PATH.test(key))
      .flatMap(([, messages]) => messages),
  );
  if (nested.size === 0) return details;
  const out: Record<string, string[]> = {};
  for (const [key, messages] of Object.entries(details)) {
    const kept = NESTED_PATH.test(key)
      ? messages
      : messages.filter(
          (message) =>
            !nested.has(message) && !(message === "is invalid" && /^(rules|dsn_source|keyfile_source|source)$/.test(key)),
        );
    if (kept.length > 0) out[key] = kept;
  }
  return out;
}

/** The form-error summary lines (secret_error_messages): "Source: …", "Rule N: …". */
export function errorMessages(details: Record<string, string[]>): string[] {
  const out: string[] = [];
  for (const [key, messages] of Object.entries(details)) {
    const rule = /^rules\[(\d+)\]\.(.+)$/.exec(key);
    const source = /^(source|dsn|keyfile)\.(.+)$/.exec(key);
    for (const message of messages) {
      if (message === "is invalid" && /^(source|dsn_source|keyfile_source|rules)$/.test(key)) continue;
      if (rule) out.push(`Rule ${Number(rule[1]) + 1}: ${fullMessage(rule[2] ?? "", message)}`);
      else if (source) out.push(`Source: ${fullMessage(source[2] ?? "", message)}`);
      else out.push(fullMessage(key, message));
    }
  }
  return out;
}

function fullMessage(attribute: string, message: string): string {
  if (attribute === "base") return message;
  const words = attribute.replace(/_/g, " ").replace(/\./g, " ");
  return `${words.charAt(0).toUpperCase()}${words.slice(1)} ${message}`;
}

export interface ClientValidation {
  /** Inline errors keyed by form field path. */
  fields: Record<string, string>;
  /** Summary lines (same style as the server summary). */
  messages: string[];
}

/**
 * Client-side checks for input the API would reject late or silently drop:
 * required fields, a source with no reference, a new inline source with no
 * value, label/setting rows with a value but no key (dropped on save), and
 * rule rows with methods or paths but no host or CIDR.
 */
export function validateForm(kind: FormKind, values: SecretFormValues, hasStoredInline: boolean): ClientValidation {
  const fields: Record<string, string> = {};
  const messages: string[] = [];
  const add = (field: string | null, message: string, summary: string) => {
    if (field) fields[field] = message;
    messages.push(summary);
  };

  if (kind === "pg_dsn") {
    if (!values.foreign_id.trim()) add("foreign_id", "can't be blank", "Foreign ID can't be blank");
    if (!values.database.trim()) add("database", "can't be blank", "Database can't be blank");
  }
  if (kind === "gcp_auth" && splitTerms(values.scopes).length === 0) {
    add("scopes", "must include at least one scope", "Scopes must include at least one scope");
  }

  const usesSource = !(kind === "gcp_auth" && values.credential_mode === "workload_identity");
  if (usesSource) {
    const type = values.source.source_type;
    if (type === "control_plane") {
      if (values.source.secret === "" && !hasStoredInline) {
        add("source.secret", "can't be blank", "Source: Inline value can't be blank");
      }
    } else if (SOURCE_REF_KEYS[type] && !values.source.reference.trim()) {
      const label = REFERENCE_LABELS[type] ?? "Reference";
      add("source.reference", "can't be blank", `Source: ${label} can't be blank`);
    }
  }

  values.labels.forEach((row, index) => {
    if (!row.key.trim() && row.value.trim()) add(null, "", `Label ${index + 1}: key can't be blank`);
  });

  if (kind === "pg_dsn") {
    values.settings.forEach((setting, index) => {
      if (!setting.name.trim() && setting.value.trim()) add(null, "", `Setting ${index + 1}: name can't be blank`);
    });
  } else {
    values.rules.forEach((rule, index) => {
      const target = rule.host.trim() || rule.cidr.trim();
      if (!target && (rule.http_methods.length > 0 || rule.paths.length > 0)) {
        add(null, "", `Rule ${index + 1}: set a host or a CIDR`);
      }
    });
    if (kind === "gcp_id_token" && rulesPayload(values.rules).length === 0) {
      add(null, "", "Rules must include at least one rule");
    }
  }

  return { fields, messages };
}
