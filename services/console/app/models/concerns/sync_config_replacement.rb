module SyncConfigReplacement
  ASSOCIATION_CLASSES = {
    source: SecretSource,
    sources: SecretSource,
    keyfile_source: SecretSource,
    dsn_source: SecretSource,
    rules: RequestRule
  }.freeze

  module_function

  def equivalent?(record, attributes, associations)
    current_document(record, attributes, associations) == replacement_document(record, attributes, associations)
  end

  def current_document(record, attributes, associations)
    document(
      record,
      attribute_names(attributes),
      associations.to_h { |association, _| [ association, record.public_send(association) ] }
    )
  end

  def replacement_document(record, attributes, associations)
    replacement = record.dup
    replacement.assign_attributes(attributes)
    document(replacement, attribute_names(attributes), associations)
  end

  def document(record, attribute_names, associations)
    normalize(
      "record" => attribute_names.index_with { |name| record.public_send(name) },
      "associations" => associations.to_h do |name, association|
        [ name, association_document(name, association) ]
      end
    )
  end

  def association_document(name, association)
    record_class = ASSOCIATION_CLASSES.fetch(name.to_sym)

    return collection_document(Array(association), record_class) if association.is_a?(Array) ||
                                                                    association.is_a?(ActiveRecord::Associations::CollectionProxy)
    return nil if association.nil?

    record_document(association, record_class)
  end

  def collection_document(records, record_class)
    documents = records.map { |record| record_document(record, record_class) }
    return documents unless record_class == SecretSource

    documents.sort_by { |document| JSON.generate(normalize(document)) }
  end

  def record_document(record, record_class)
    record = record_class.new(record.to_h) unless record.is_a?(record_class)
    association_attribute_names(record_class).index_with { |name| record.public_send(name) }
  end

  def association_attribute_names(record_class)
    record_class.const_get(:SYNC_CONFIG_REPLACEMENT_ATTRIBUTES)
  end

  def attribute_names(attributes)
    attributes.to_h.keys.map(&:to_s)
  end

  def normalize(value)
    case value
    when Hash
      value.to_h.each_with_object({}) do |(key, nested), normalized|
        normalized[key.to_s] = normalize(nested)
      end.sort.to_h
    when Array
      value.map { |nested| normalize(nested) }
    else
      value
    end
  end
end
