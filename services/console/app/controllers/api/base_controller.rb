module Api
  class BaseController < ActionController::API
    before_action :authenticate_api_key!

    rescue_from ActiveRecord::RecordNotFound, with: :render_not_found
    rescue_from ActionController::ParameterMissing, with: :render_bad_request
    rescue_from ActionController::BadRequest, with: :render_bad_request

    attr_reader :current_api_key

    def current_user
      current_api_key&.user
    end

    private

    def authenticate_api_key!
      token = bearer_token
      @current_api_key = ApiKey.find_by_token(token) if token.present?
      return if @current_api_key

      render_error(status: :unauthorized, message: "invalid or missing API key")
    end

    def bearer_token
      header = request.headers["Authorization"].to_s
      return nil unless header.start_with?("Bearer ")
      header.sub(/\ABearer\s+/, "").presence
    end

    def render_error(status:, message:, details: nil)
      body = { error: { message: message } }
      body[:error][:details] = details if details
      render status: status, json: body
    end

    def render_not_found(e)
      render_error(status: :not_found, message: e.message)
    end

    def render_bad_request(e)
      render_error(status: :bad_request, message: e.message)
    end

    def render_validation_error(record)
      render_error(status: :unprocessable_entity, message: "validation failed",
                   details: record.errors.as_json)
    end

    def data_params
      params.require(:data)
    end

    # Namespace for a create/upsert write, taken from the request body
    # (defaults to "default"), matching the create path.
    def upsert_namespace
      data_params[:namespace].presence || "default"
    end

    # Permits the body of a document write (create or PUT upsert) with replace
    # semantics: a permitted field that is omitted from the body, or sent as
    # null (which strong params drops for hash and array filters), is reset to
    # its column default rather than retained from the existing record, so the
    # body always replaces the whole document. The identity columns (namespace,
    # foreign_id) are the exception: an upsert by foreign_id sets them on the
    # record before assignment, so a blank body value must not wipe them.
    def permit_document(ref, attrs, *scalars, **filters)
      permitted = attrs.permit(:namespace, :foreign_id, *scalars, **filters)
      permitted.delete(:foreign_id) if permitted[:foreign_id].blank? && ref.foreign_id.present?
      permitted.delete(:namespace) if permitted[:namespace].blank? && ref.namespace.present?
      permitted[:namespace] = "default" if permitted[:namespace].blank? && ref.namespace.blank?

      defaults = ref.class.column_defaults
      columns = (scalars + filters.keys).map(&:to_s)
      permitted.with_defaults(columns.index_with { |c| defaults[c] })
    end

    # Resolves the target of a PUT/PATCH write so the verb behaves as an upsert.
    #
    # When :id is an opaque id for this model it must reference an existing
    # record (update only; ActiveRecord::RecordNotFound otherwise). Any other
    # value is treated as a foreign_id within the body namespace and the record
    # is initialized when absent, so a PUT to a foreign_id creates it. The
    # identity columns come from the URL/body here rather than mass assignment,
    # and a foreign_id can never start with the opaque-id prefix (model
    # validation), so the two identifier forms stay unambiguous.
    def resolve_for_upsert(model)
      identifier = params[:id].to_s
      if identifier.start_with?("#{model.oid_prefix}_")
        model.find_by_oid!(identifier)
      else
        record = model.find_or_initialize_by(namespace: upsert_namespace, foreign_id: identifier)
        record.created_by = current_user if record.new_record?
        record
      end
    end

    # Resolves a record from a namespaced lookup route, where namespace and
    # foreign_id are explicit, required path segments. Raises
    # ActiveRecord::RecordNotFound when nothing matches.
    def find_by_foreign_id!(model)
      model.find_by!(namespace: params.require(:namespace), foreign_id: params.require(:foreign_id))
    end

    DEFAULT_PAGE_LIMIT = 50
    MAX_PAGE_LIMIT = 200

    def paginated_label_search(scope)
      namespace = params.require(:namespace)

      labels = label_filter_params
      filtered = scope.where(namespace: namespace)
      filtered = filtered.where("labels @> ?", labels.to_json) if labels.any?

      limit = pagination_limit
      page = pagination_page
      total = filtered.count
      records = filtered.order(created_at: :asc, id: :asc).limit(limit).offset((page - 1) * limit)

      total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)
      meta = { page: page, limit: limit, total: total, total_pages: total_pages }
      [ records, meta ]
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
