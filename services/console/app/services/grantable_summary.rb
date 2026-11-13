# Identifying metadata for a grantable secret, shared by the grant payloads and
# the cross-kind secret list. Never includes sources, values, or config.
module GrantableSummary
  KIND_BY_MODEL = SecretKinds::SECRET_KINDS.to_h { |kind, config| [ config.fetch(:model), kind ] }.freeze

  module_function

  def kind_for(secret)
    KIND_BY_MODEL.fetch(secret.class)
  end

  def call(secret)
    kind = kind_for(secret)
    {
      kind: kind,
      kind_label: SecretKinds::SECRET_KINDS.dig(kind, :label),
      id: secret.oid,
      foreign_id: secret.foreign_id,
      name: secret.name
    }
  end
end
