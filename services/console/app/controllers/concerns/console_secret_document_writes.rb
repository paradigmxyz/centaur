# The console API's write semantics for the secret kinds it edits, on top of
# SecretDocumentWrites (include into the Api::V1::Console subclass):
#
# - a PATCH that omits an inline (control_plane) source's `secret` keeps the
#   stored value, so the SPA never needs to read plaintext back;
# - 422 details add path-style keys for nested errors ("source.config",
#   "keyfile.secret", "rules[0].cidr");
# - an explicit `foreign_id: null` clears the Foreign ID of an existing secret
#   addressed by its opaque id (an omitted key or blank string keeps it; a
#   kind that requires one gets a 422).
module ConsoleSecretDocumentWrites
  extend ActiveSupport::Concern

  private

  def retain_omitted_inline_secret?
    true
  end

  def nested_error_path_keys?
    true
  end

  def explicit_foreign_id_clear?(ref, attrs)
    ref.persisted? && params[:id].to_s == ref.oid &&
      attrs.key?(:foreign_id) && attrs[:foreign_id].nil?
  end
end
