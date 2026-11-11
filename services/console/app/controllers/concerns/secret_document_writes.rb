# Shared write helpers for the secret kinds the console edits (static, pg_dsn,
# gcp_auth, gcp_id_token):
#
# - #build_secret_source builds the replacement SecretSource from a payload key.
#   On PATCH, an inline (control_plane) source that omits `secret` keeps the
#   stored value, so clients never need to read plaintext back to save other
#   edits. PUT keeps its full-replace contract and still requires `secret`.
# - #validate_nested_secret_records! validates the nested source and rules
#   before they are assigned and reports their errors under path-style detail
#   keys ("source.config", "rules[0].cidr"). Without it, an invalid has_one
#   source is silently dropped on create and fails mid-write on update.
module SecretDocumentWrites
  extend ActiveSupport::Concern

  private

  # +existing+ is the record's current source (nil on create).
  def build_secret_source(attrs, key, existing)
    return nil unless attrs.key?(key) && attrs[key].present?

    source_attrs = attrs.require(key).permit(:source_type, :secret, config: {}).to_h
    if retain_inline_secret?(source_attrs, existing)
      source_attrs["secret"] = existing.secret
    end
    SecretSource.new(source_attrs)
  end

  # Validates the nested source(s) (keyed by payload name) and rules before
  # anything is assigned to +record+, and raises ActiveRecord::RecordInvalid
  # (for render_validation_error) carrying their errors on +record+. Assigning
  # an invalid has_one/has_many to a persisted record would otherwise fail
  # mid-write instead of returning a 422.
  def validate_nested_secret_records!(record, sources: {}, rules: [])
    errors = []
    sources.each do |name, source|
      next if source.nil? || source.valid?

      source.errors.each { |error| errors << [ :"#{name}.#{error.attribute}", error.message ] }
    end
    Array(rules).each_with_index do |rule, index|
      next if rule.valid?

      rule.errors.each { |error| errors << [ :"rules[#{index}].#{error.attribute}", error.message ] }
    end
    return if errors.empty?

    errors.each { |attribute, message| record.errors.add(attribute, message) }
    raise ActiveRecord::RecordInvalid, record
  end

  def retain_inline_secret?(source_attrs, existing)
    request.patch? &&
      source_attrs["source_type"] == "control_plane" &&
      source_attrs["secret"].blank? &&
      existing&.persisted? &&
      existing.source_type == "control_plane" &&
      existing.secret.present?
  end
end
