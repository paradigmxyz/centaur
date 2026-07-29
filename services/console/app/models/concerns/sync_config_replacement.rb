module SyncConfigReplacement
  REPLACEMENT_ASSOCIATION_CLASSES = [ SecretSource, RequestRule ].freeze

  module_function

  def equivalent?(record, attributes, associations)
    validate_association_coverage!(record, associations)
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
        reflection = record.class.reflect_on_association(name)
        [ name, association_document(reflection, association) ]
      end
    )
  end

  def association_document(reflection, association)
    raise ArgumentError, "unknown association" unless reflection

    return collection_document(Array(association), reflection.klass) if reflection.collection?
    return nil if association.nil?

    record_document(association, reflection.klass)
  end

  def collection_document(records, record_class)
    documents = records.map { |record| record_document(record, record_class) }
    return documents unless record_class == SecretSource

    documents.sort_by { |document| JSON.generate(normalize(document)) }
  end

  def record_document(record, record_class)
    raise ArgumentError, "unsupported association class #{record_class.name}" unless
      REPLACEMENT_ASSOCIATION_CLASSES.include?(record_class)

    record = record_class.new(record.to_h) unless record.is_a?(record_class)
    association_attribute_names(record_class).index_with { |name| record.public_send(name) }
  end

  def association_attribute_names(record_class)
    record_class.const_get(:SYNC_CONFIG_REPLACEMENT_ATTRIBUTES)
  end

  def validate_association_coverage!(record, associations)
    expected = record.class.reflect_on_all_associations.filter_map do |reflection|
      next if reflection.polymorphic?

      reflection.name if REPLACEMENT_ASSOCIATION_CLASSES.include?(reflection.klass)
    end
    missing = expected - associations.keys.map(&:to_sym)
    return if missing.empty?

    raise ArgumentError, "missing replacement associations: #{missing.join(", ")}"
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
