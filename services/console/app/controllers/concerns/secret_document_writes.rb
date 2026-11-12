# Shared write helpers for the secret kinds the console edits (static, pg_dsn,
# gcp_auth, gcp_id_token):
#
# - #build_secret_source builds the replacement SecretSource from a payload key.
#   On PATCH, an inline (control_plane) source that omits `secret` keeps the
#   stored value, so clients never need to read plaintext back to save other
#   edits. PUT keeps its full-replace contract and still requires `secret`.
#   Callers take #lock_secret_document! inside their write transaction before
#   building the source, so the retained value is the one stored when the
#   lock was granted and a concurrent rotation is never overwritten.
# - #validate_nested_secret_records! turns invalid nested sources and rules
#   into a 422 (render_validation_error) instead of a silent drop or a
#   mid-write exception. The 422 `details` keep the keys earlier versions
#   returned (see below) and add path-style keys for every nested error
#   ("source.config", "keyfile.secret", "rules[0].cidr").
module SecretDocumentWrites
  extend ActiveSupport::Concern

  private

  # Row-locks (SELECT ... FOR UPDATE) and reloads a persisted document. Call
  # inside the write transaction before reading any stored value that the
  # write retains, so the read happens after a concurrent write commits.
  def lock_secret_document!(record)
    record.lock! if record.persisted?
  end

  # +existing+ is the record's current source (nil on create).
  def build_secret_source(attrs, key, existing)
    return nil unless attrs.key?(key) && attrs[key].present?

    source_attrs = attrs.require(key).permit(:source_type, :secret, config: {}).to_h
    if retain_inline_secret?(source_attrs, existing)
      source_attrs["secret"] = existing.secret
    end
    SecretSource.new(source_attrs)
  end

  # Validates the nested source(s) (keyed by payload name, e.g. { keyfile: src })
  # and rules, and raises ActiveRecord::RecordInvalid carrying their errors on
  # +record+ when any is invalid.
  #
  # prepare: (association mode) assigns the document to a new +record+ in
  # memory so its own validations run as they did on create.
  #
  # legacy: selects the pre-existing (compatibility) keys added alongside the
  # path keys:
  # - :flat (static secrets, called after the parent saved): the first invalid
  #   nested record's own attribute keys, in save order (source, then rules),
  #   exactly as the nested save! used to report them ({ "cidr" => [...] }).
  # - :association (pg_dsn, gcp_auth, gcp_id_token, called before assignment):
  #   the parent's own validation errors plus "rules" => ["is invalid"] (the
  #   has_many autosave error), and "<association>" => ["is invalid"] for an
  #   invalid source (e.g. "keyfile_source").
  def validate_nested_secret_records!(record, legacy:, sources: {}, rules: [], source_associations: {}, prepare: nil)
    invalid_sources = sources.select { |_name, source| source && !source.valid? }
    invalid_rules = Array(rules).each_with_index.reject { |rule, _index| rule.valid? }
    return if invalid_sources.empty? && invalid_rules.empty?

    case legacy
    when :flat
      first = invalid_sources.values.first || invalid_rules.first&.first
      first.errors.each { |error| record.errors.add(error.attribute, error.message) }
    when :association
      parent_errors_for(record, prepare).each { |attribute, message| record.errors.add(attribute, message) }
      invalid_sources.each_key do |name|
        record.errors.add(source_associations.fetch(name, name), "is invalid")
      end
      record.errors.add(:rules, "is invalid") if invalid_rules.any?
    end

    invalid_sources.each do |name, source|
      source.errors.each { |error| record.errors.add(:"#{name}.#{error.attribute}", error.message) }
    end
    invalid_rules.each do |rule, index|
      rule.errors.each { |error| record.errors.add(:"rules[#{index}].#{error.attribute}", error.message) }
    end
    raise ActiveRecord::RecordInvalid, record
  end

  # The parent's own validation errors, as a create used to report them next
  # to "rules is invalid". Only for a new record, after +prepare+ assigns the
  # document in memory (nothing is saved). Updates used to fail mid-write, so
  # there is no earlier shape to keep.
  def parent_errors_for(record, prepare)
    return [] if record.persisted?

    prepare&.call
    record.valid?
    record.errors.reject { |error| %i[rules keyfile_source dsn_source].include?(error.attribute) }
          .map { |error| [ error.attribute, error.message ] }
          .tap { record.errors.clear }
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
