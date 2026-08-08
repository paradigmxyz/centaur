class PrincipalIdentityLabels
  FIELDS = {
    "kind" => { column: "kind" }.freeze,
    "slack_user_id" => { column: "slack_user_id" }.freeze,
    "slack_channel_id" => { column: "slack_channel_id" }.freeze,
    "slack_team_id" => { column: "slack_team_id" }.freeze,
    "slack_email" => { column: "slack_email" }.freeze,
    "console-user-id" => { column: "console_user_id", kind: "console_user" }.freeze,
    "email" => { column: "console_user_email", kind: "console_user" }.freeze
  }.freeze

  class << self
    def columns
      FIELDS.values.map { |field| field.fetch(:column) }.uniq
    end

    def labels_for(kind)
      fields_for(kind).keys
    end

    def serialize(principal)
      fields_for(principal.kind).keys.to_h do |label|
        [ label, value(principal, label) ]
      end.compact
    end

    def assign(principal)
      return unless principal.will_save_change_to_labels?

      assign_label(principal, "kind")
      fields_for(principal.kind).each_key do |label|
        assign_label(principal, label) unless label == "kind"
      end
    end

    def validate_consistency(principal)
      labels = principal.labels.to_h
      kind = effective_kind(principal, labels)

      fields_for(kind).each do |label, field|
        column = field.fetch(:column)
        next unless labels.key?(label) && principal.will_save_change_to_attribute?(column)

        add_conflict_error(principal, column, label) unless principal.public_send(column) == decode_assignment_value(label, labels[label])
      end
    end

    def validate_request_consistency(principal, attributes)
      attributes = attributes.to_unsafe_h if attributes.respond_to?(:to_unsafe_h)
      attributes = attributes.to_h.stringify_keys
      raw_labels = attributes.fetch("labels", {})
      labels = raw_labels.respond_to?(:to_h) ? raw_labels.to_h.stringify_keys : {}
      kind = if attributes.key?("kind")
        attributes["kind"]
      elsif labels.key?("kind")
        labels["kind"]
      else
        principal.kind
      end

      fields_for(kind).each do |label, field|
        column = field.fetch(:column)
        next unless attributes.key?(column) && labels.key?(label)

        direct_value = principal.class.type_for_attribute(column).cast(attributes[column])
        label_value = decode_assignment_value(label, labels[label])
        add_conflict_error(principal, column, label) unless direct_value == label_value
      end
    end

    def strip(principal)
      principal[:labels] = principal.labels.to_h.except(*labels_for(principal.kind))
    end

    def promoted?(principal, label)
      fields_for(principal&.kind).key?(label.to_s)
    end

    def value(principal, label)
      return unless principal

      field = fields_for(principal.kind)[label.to_s]
      return unless field
      return principal.console_user&.oid if field.fetch(:column) == "console_user_id"

      principal.public_send(field.fetch(:column))
    end

    def apply_filters(scope, labels)
      remaining = labels.dup
      filtered = FIELDS.reduce(scope) do |relation, (label, field)|
        value = remaining.delete(label)
        next relation if value.blank?

        table = scope.connection.quote_table_name(scope.table_name)
        column = scope.connection.quote_column_name(field.fetch(:column))
        stored_value = decode_filter_value(label, value)
        label_filter = { label => value }.to_json
        relation.where(filter_sql(table, column, stored_value), *filter_values(stored_value, label_filter))
      end

      remaining.any? ? filtered.where("labels @> ?", remaining.to_json) : filtered
    end

    private

    def effective_kind(principal, labels)
      return principal.kind if principal.will_save_change_to_kind? || !labels.key?("kind")

      decode_assignment_value("kind", labels["kind"])
    end

    def add_conflict_error(principal, column, label)
      principal.errors.add(column, "does not agree with labels.#{label}")
    end

    def fields_for(kind)
      FIELDS.select { |_label, field| field[:kind].nil? || field[:kind] == kind }
    end

    def assign_label(principal, label)
      field = FIELDS.fetch(label).fetch(:column)
      return if principal.will_save_change_to_attribute?(field) || !principal.labels.to_h.key?(label)

      principal[field] = decode_assignment_value(label, principal.labels.to_h[label])
    end

    def decode_assignment_value(label, value)
      return value if label == "kind"
      return User.find_by_oid(value)&.id if label == "console-user-id"

      value.presence
    end

    def decode_filter_value(label, value)
      return User.decode_oid(value) if label == "console-user-id"

      value.to_s
    end

    def filter_sql(table, column, stored_value)
      return "#{table}.labels @> ?" if stored_value.nil?

      "#{table}.#{column} = ? OR #{table}.labels @> ?"
    end

    def filter_values(stored_value, label_filter)
      stored_value.nil? ? [ label_filter ] : [ stored_value, label_filter ]
    end
  end
end
