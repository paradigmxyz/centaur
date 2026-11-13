// Mirrors SecretKinds::SECRET_KINDS (app/controllers/concerns/secret_kinds.rb).

export const SECRET_KINDS = {
  static: { label: "Static", form: true },
  gcp_auth: { label: "GCP Auth", form: true },
  gcp_id_token: { label: "GCP ID Token", form: true },
  aws_auth: { label: "AWS Auth", form: false },
  oauth_token: { label: "OAuth Token", form: false },
  pg_dsn: { label: "Postgres DSN", form: true },
  hmac: { label: "HMAC", form: false },
  ssh_key: { label: "SSH key", form: false },
} as const;

export type SecretKind = keyof typeof SECRET_KINDS;

export const SECRET_KIND_SLUGS = Object.keys(SECRET_KINDS) as SecretKind[];

export function secretKindLabel(kind: string): string {
  return (SECRET_KINDS as Record<string, { label: string }>)[kind]?.label ?? kind;
}

/** Field name on POST /api/v1/console/grants for a kind, e.g. "static_secret_id". */
export function grantKey(kind: SecretKind): `${SecretKind}_secret_id` {
  return `${kind}_secret_id`;
}

/** Friendly labels for secret source backends (ConsoleController::SOURCE_TYPE_LABELS). */
export const SOURCE_TYPE_LABELS: Record<string, string> = {
  env: "Env",
  aws_sm: "AWS-SM",
  aws_ssm: "AWS-SSM",
  "1password": "1Password",
  "1password_connect": "1Password-Connect",
  control_plane: "Inline",
  token_broker: "Token-Broker",
  workload_identity: "Workload-Identity",
};

/** Summary of a secret attached to grant payloads and GET /api/v1/console/secrets. */
export interface GrantableSummary {
  kind: SecretKind;
  kind_label: string;
  id: string;
  foreign_id: string | null;
  name: string | null;
}

/** secret_option_label: "name (foreign_id|oid)". */
export function secretOptionLabel(secret: Pick<GrantableSummary, "id" | "name" | "foreign_id">): string {
  const primary = secret.name || secret.foreign_id || secret.id;
  const identifier = secret.foreign_id || secret.id;
  return identifier === primary ? primary : `${primary} (${identifier})`;
}
