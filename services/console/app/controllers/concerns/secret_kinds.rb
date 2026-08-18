# The registry of grantable secret kinds, keyed by the short slug used across the
# console UI (the secrets index, detail pages, and the create/edit forms). Shared
# by the read-only ConsoleController and the form-handling Console::SecretsController
# so both agree on the model class, label, eager-load association, and whether a
# create/edit form is implemented yet.
module SecretKinds
  extend ActiveSupport::Concern

  # `form:` gates which kinds expose a create/edit form (and so appear in the "Add
  # Secret" menu). The remaining kinds are read-only until their form is built.
  SECRET_KINDS = {
    "static" => { model: StaticSecret, label: "Static", includes: :source, form: true },
    "gcp_auth" => { model: GcpAuthSecret, label: "GCP Auth", includes: :keyfile_source, form: true },
    "gcp_id_token" => { model: GcpIdTokenSecret, label: "GCP ID Token", includes: :keyfile_source, form: true },
    "aws_auth" => { model: AwsAuthSecret, label: "AWS Auth", includes: :sources, form: false },
    "oauth_token" => { model: OauthTokenSecret, label: "OAuth Token", includes: :sources, form: false },
    "pg_dsn" => { model: PgDsnSecret, label: "Postgres DSN", includes: :dsn_source, form: true },
    "hmac" => { model: HmacSecret, label: "HMAC", includes: :sources, form: false }
  }.freeze

  # The config key that carries a source's human-meaningful reference, per
  # source_type. control_plane keeps its value inline (and redacted), so it has no
  # reference key. Used by the console detail view and the source form fields.
  SOURCE_REF_KEYS = {
    "env" => "var", "aws_sm" => "secret_id", "aws_ssm" => "name",
    "1password" => "secret_ref", "1password_connect" => "secret_ref",
    "token_broker" => "credential_id"
  }.freeze

  included do
    helper_method :secret_kind_label, :secret_form_kinds
  end

  def secret_kind_label(slug)
    SECRET_KINDS.dig(slug, :label) || slug
  end

  # Kinds with an implemented create/edit form, in registry order. Drives the
  # "Add Secret" dropdown on the secrets index.
  def secret_form_kinds
    SECRET_KINDS.select { |_, cfg| cfg[:form] }
  end
end
