module SyncConfigReplacement
  SOURCE_ATTRIBUTES = %w[source_type config secret role role_kind].freeze
  RULE_ATTRIBUTES = %w[position host cidr http_methods paths].freeze

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

    documents.sort_by { |document| JSON.generate(document) }
  end

  def record_document(record, record_class)
    fields = case record_class.name
    when "SecretSource" then SOURCE_ATTRIBUTES
    when "RequestRule" then RULE_ATTRIBUTES
    else raise ArgumentError, "unsupported association class #{record_class.name}"
    end

    attributes = if record.is_a?(record_class)
      fields.index_with { |name| record.public_send(name) }
    else
      normalize(record.to_h)
    end

    fields.index_with { |field| attributes[field] }.tap do |document|
      document["config"] ||= {} if record_class == SecretSource
      if record_class == RequestRule
        document["http_methods"] ||= []
        document["paths"] ||= []
      end
    end
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
