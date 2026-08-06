module Api
  class BaseController < ActionController::API
    include ApiRequestSupport

    before_action :authenticate_api_key!
    before_action :normalize_namespace_compatibility!

    rescue_from ActiveRecord::RecordNotFound, with: :render_not_found

    attr_reader :current_api_key

    def current_user
      current_api_key&.user
    end

    private

    def authenticate_api_key!
      token = bearer_token
      @current_api_key = ApiKey.find_by_token(token) if token.present?
      unless current_api_key&.user&.active?
        return render_error(status: :unauthorized, message: "invalid or missing API key")
      end
      return if current_api_key.user.admin?

      render_error(status: :forbidden, message: "API key owner is not an admin")
    end

    def render_not_found(e)
      render_error(status: :not_found, message: e.message)
    end

    def render_validation_error(record)
      render_error(status: :unprocessable_entity, message: "validation failed",
                   details: record.errors.as_json)
    end

    def data_params
      params.require(:data)
    end

    # Permits the body of a document write (create or PUT upsert) with replace
    # semantics: a permitted field that is omitted from the body, or sent as
    # null (which strong params drops for hash and array filters), is reset to
    # its column default rather than retained from the existing record, so the
    # body always replaces the whole document. The identity column foreign_id
    # is the exception: an upsert by foreign_id sets it on the
    # record before assignment, so a blank body value must not wipe them.
    def permit_document(ref, attrs, *scalars, **filters)
      permitted = attrs.permit(:foreign_id, *scalars, **filters)
      permitted.delete(:foreign_id) if permitted[:foreign_id].blank? && ref.foreign_id.present?

      defaults = ref.class.column_defaults
      columns = (scalars + filters.keys).map(&:to_s)
      permitted.with_defaults(columns.index_with { |c| defaults[c] })
    end

    # Resolves the target of a PUT/PATCH write so the verb behaves as an upsert.
    #
    # When :id is an opaque id for this model it must reference an existing
    # record (update only; ActiveRecord::RecordNotFound otherwise). Any other
    # value is treated as a globally unique foreign_id and the record
    # is initialized when absent, so a PUT to a foreign_id creates it. The
    # identity columns come from the URL/body here rather than mass assignment,
    # and a foreign_id can never start with the opaque-id prefix (model
    # validation), so the two identifier forms stay unambiguous.
    def resolve_for_upsert(model)
      identifier = params[:id].to_s
      if identifier.start_with?("#{model.oid_prefix}_")
        model.find_by_oid!(identifier)
      else
        record = model.find_or_initialize_by(foreign_id: identifier)
        record.created_by = current_user if record.new_record?
        record
      end
    end

    # Resolves a record from a canonical or default-namespace compatibility
    # lookup route.
    def find_by_foreign_id!(model)
      model.find_by!(foreign_id: params.require(:foreign_id))
    end

    def build_rules(attrs)
      request_rule_attributes(attrs).map { |rule_attrs| RequestRule.new(rule_attrs) }
    end

    def request_rule_attributes(attrs)
      Array(attrs[:rules]).each_with_index.map do |rule, position|
        rule_params = if rule.is_a?(ActionController::Parameters)
          rule
        else
          ActionController::Parameters.new(rule || {})
        end

        rule_params.permit(:host, :cidr, http_methods: [], paths: []).to_h.merge(position: position)
      end
    end

    def with_sync_config_replacement_guard(record, attributes, **associations)
      record.lock! unless record.new_record?
      return record if !record.new_record? && SyncConfigReplacement.equivalent?(record, attributes, associations)

      yield
    end

    DEFAULT_PAGE_LIMIT = 50
    MAX_PAGE_LIMIT = 200

    def paginated_label_search(scope, label_filter: nil)
      labels = label_filter_params
      filtered = if label_filter
        label_filter.call(scope, labels)
      elsif labels.any?
        scope.where("labels @> ?", labels.to_json)
      else
        scope
      end

      limit = pagination_limit
      page = pagination_page
      total = filtered.count
      records = filtered.order(created_at: :asc, id: :asc).limit(limit).offset((page - 1) * limit)

      total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)
      meta = { page: page, limit: limit, total: total, total_pages: total_pages }
      [ records, meta ]
    end

    NAMESPACE_ERROR =
      'namespace must be "default"; additional namespaces are deprecated and will be removed on September 4, 2026'.freeze

    # Keep one-release request compatibility for clients that still send the
    # removed fields. `default` is accepted and discarded; every other value is
    # a hard error. Non-default lookup paths do not route and therefore return 404.
    def normalize_namespace_compatibility!
      invalid = []
      [ params, params[:data] ].compact.each do |container|
        discard_legacy_namespace!(container, "namespace", invalid)
        discard_legacy_namespace!(container, "credential_namespace", invalid)
      end
      discard_token_broker_namespaces!(params[:data], invalid)
      return if invalid.empty?

      render_error(status: :unprocessable_entity, message: NAMESPACE_ERROR)
    end

    LEGACY_SINGLE_SOURCE_FIELDS = %w[
      source
      keyfile
      dsn
      access_key_id
      secret_access_key
      session_token
    ].freeze
    LEGACY_SOURCE_COLLECTION_FIELDS = %w[credentials token_endpoint_headers].freeze

    def discard_token_broker_namespaces!(data, invalid)
      return unless data.respond_to?(:[])

      LEGACY_SINGLE_SOURCE_FIELDS.each do |field|
        discard_token_broker_namespace!(data[field], invalid)
      end
      LEGACY_SOURCE_COLLECTION_FIELDS.each do |field|
        collection = data[field]
        next unless collection.respond_to?(:each_value)

        collection.each_value do |source|
          discard_token_broker_namespace!(source, invalid)
        end
      end
    end

    def discard_token_broker_namespace!(source, invalid)
      return unless source.respond_to?(:[])
      return unless source["source_type"] == "token_broker"

      discard_legacy_namespace!(source["config"], "credential_namespace", invalid)
    end

    def discard_legacy_namespace!(container, field, invalid)
      return unless container.respond_to?(:key?) && container.key?(field)

      value = container.delete(field)
      invalid << value unless value.blank? || value == "default"
    end

    def label_filter_params
      raw = params[:labels]
      return {} if raw.blank?
      unless raw.is_a?(ActionController::Parameters) || raw.is_a?(Hash)
        raise ActionController::BadRequest, "labels must be a hash of key=value pairs"
      end
      hash = raw.respond_to?(:to_unsafe_h) ? raw.to_unsafe_h : raw.to_h
      hash.each do |k, v|
        unless v.is_a?(String) || v.is_a?(Numeric) || v == true || v == false
          raise ActionController::BadRequest, "label value for #{k} must be a scalar"
        end
      end
      hash
    end

    def pagination_limit
      raw = params[:limit].presence
      return DEFAULT_PAGE_LIMIT unless raw
      n = Integer(raw, 10)
      n.clamp(1, MAX_PAGE_LIMIT)
    rescue ArgumentError, TypeError
      raise ActionController::BadRequest, "limit must be an integer"
    end

    def pagination_page
      raw = params[:page].presence
      return 1 unless raw
      n = Integer(raw, 10)
      n < 1 ? 1 : n
    rescue ArgumentError, TypeError
      raise ActionController::BadRequest, "page must be an integer"
    end
  end
end
