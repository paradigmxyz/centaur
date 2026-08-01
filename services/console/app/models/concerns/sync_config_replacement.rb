module SyncConfigReplacement
  # Association values must be model instances (or arrays of them) whose class
  # defines SYNC_CONFIG_REPLACEMENT_ATTRIBUTES — controllers pass the same
  # instances they persist when the replacement turns out not to be a no-op.
  module_function

  def equivalent?(record, attributes, associations)
    replacement = record.dup
    replacement.assign_attributes(attributes)
    names = attributes.to_h.keys.map(&:to_s)
    return false unless names.index_with { |name| record.public_send(name).as_json } ==
                        names.index_with { |name| replacement.public_send(name).as_json }

    associations.all? do |name, replacement_records|
      documents(record.public_send(name)) == documents(replacement_records)
    end
  end

  # Hash#== ignores key order, so as_json (deep string keys) is the only
  # normalization needed. Sorting by role/position is equality-preserving
  # because both are part of the document: roles are unique within a secret's
  # sources and rule positions are assigned 0..n from the request body.
  def documents(records)
    Array(records)
      .map { |record| record.slice(*record.class::SYNC_CONFIG_REPLACEMENT_ATTRIBUTES).as_json }
      .sort_by { |document| [ document["role"].to_s, document["position"].to_i ] }
  end
end
