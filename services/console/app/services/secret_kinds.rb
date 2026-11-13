# The registry of grantable secret kinds, keyed by the short slug used across the
# console (GET /api/v1/secrets, the secret detail and role-grant endpoints, and
# the SPA's secret pages, which mirror it in web/src/lib/secret-kinds.ts) so all
# agree on the model class, label, eager-load association, and whether the
# console offers a create/edit form for the kind.
module SecretKinds
  # `form:` marks the kinds the console can create and edit (the "Add Secret"
  # menu; `editable` on GET /api/v1/secrets/:kind/:id). The remaining kinds are
  # read-only in the console and managed through their /api/v1 resources.
  SECRET_KINDS = {
    "static" => { model: StaticSecret, label: "Static", includes: :source, form: true },
    "gcp_auth" => { model: GcpAuthSecret, label: "GCP Auth", includes: :keyfile_source, form: true },
    "gcp_id_token" => { model: GcpIdTokenSecret, label: "GCP ID Token", includes: :keyfile_source, form: true },
    "aws_auth" => { model: AwsAuthSecret, label: "AWS Auth", includes: :sources, form: false },
    "oauth_token" => { model: OauthTokenSecret, label: "OAuth Token", includes: :sources, form: false },
    "pg_dsn" => { model: PgDsnSecret, label: "Postgres DSN", includes: :dsn_source, form: true },
    "hmac" => { model: HmacSecret, label: "HMAC", includes: :sources, form: false },
    "ssh_key" => { model: SshKeySecret, label: "SSH key", includes: :key_source, form: false }
  }.freeze

  # The config key that carries a source's human-meaningful reference, per
  # source_type. control_plane keeps its value inline (and redacted), so it has no
  # reference key. Used by SecretConsoleView (the secret detail endpoint).
  SOURCE_REF_KEYS = {
    "env" => "var", "aws_sm" => "secret_id", "aws_ssm" => "name",
    "1password" => "secret_ref", "1password_connect" => "secret_ref",
    "token_broker" => "credential_id"
  }.freeze

  # The secret of +kind+ with opaque id +id+, with its sources eager loaded.
  # Raises ActiveRecord::RecordNotFound for an unknown kind or id.
  def self.find_secret!(kind, id)
    config = SECRET_KINDS[kind.to_s]
    raise ActiveRecord::RecordNotFound, "secret not found" unless config

    config[:model].includes(config[:includes]).find_by_oid!(id)
  end
end
