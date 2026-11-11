module Api
  # Base for the operator JSON API under /api/v1.
  #
  # A request authenticates with EITHER a bearer ApiKey (machine callers, CLIs)
  # OR the console session cookie (the browser SPA). An Authorization header,
  # when present, is authoritative: a bad bearer token never falls back to the
  # cookie. Cookie-authenticated requests that are not GET/HEAD must carry the
  # session's CSRF token in X-CSRF-Token (fetch it from GET /api/v1/me); bearer
  # requests skip CSRF because browsers never attach them ambiently.
  #
  # Authorization is per endpoint. Every action is admin-only unless the
  # controller opts in with `allow_active_users` (any active account; scope
  # records with `owned_scope` / `require_owner_or_admin!`) or
  # `allow_pending_users` (also pending accounts, e.g. GET /api/v1/me). For
  # session requests the admin check honours the "view as operator" descope.
  class BaseController < ActionController::API
    include ApiRequestSupport
    include ConsoleSession
    include ActionController::RequestForgeryProtection

    CSRF_SAFE_METHODS = %w[GET HEAD].freeze

    # The railtie only applies forgery-protection defaults to
    # ActionController::Base; share its Origin-header check with the API.
    self.forgery_protection_origin_check = ActionController::Base.forgery_protection_origin_check

    before_action :authenticate_request!
    before_action :require_active_account!
    before_action :verify_session_csrf_token!
    before_action :require_admin!

    rescue_from ActiveRecord::RecordNotFound, with: :render_not_found

    attr_reader :current_api_key

    # Opts actions into any active (approved, not disabled) account instead of
    # the admin-only default. The action must scope what it loads to the
    # caller unless acting_admin?.
    def self.allow_active_users(**options)
      skip_before_action :require_admin!, **options
    end

    # Opts actions into pending accounts as well (session requests only; a
    # pending API key owner is still rejected). Implies allow_active_users.
    def self.allow_pending_users(**options)
      skip_before_action :require_admin!, **options
      skip_before_action :require_active_account!, **options
    end

    def current_user
      return current_api_key&.user if api_key_authenticated?

      console_session_user if session_authenticated?
    end

    private

    # :api_key, :session, or nil before authentication.
    attr_reader :auth_method

    def api_key_authenticated?
      auth_method == :api_key
    end

    def session_authenticated?
      auth_method == :session
    end

    # Admin check for authorization decisions. Bearer callers act with their
    # owner's admin flag; session callers lose it while descoped.
    def acting_admin?
      return false unless current_user&.admin?

      !descoped?
    end

    def descoped?
      session_authenticated? && console_session_descoped?
    end

    def authenticate_request!
      if request.headers["Authorization"].present?
        authenticate_api_key!
      elsif console_session_user
        authenticate_session!
      else
        render_error(status: :unauthorized, message: "invalid or missing API key")
      end
    end

    def authenticate_api_key!
      token = bearer_token
      @current_api_key = ApiKey.find_by_token(token) if token.present?
      unless current_api_key&.user&.active?
        return render_error(status: :unauthorized, message: "invalid or missing API key")
      end

      @auth_method = :api_key
    end

    # A disabled session user is signed out on every endpoint, including the
    # ones open to pending accounts.
    def authenticate_session!
      if console_session_user.disabled?
        reset_session
        return render_error(status: :unauthorized, message: "account disabled",
                            details: { reason: "account_disabled" })
      end

      @auth_method = :session
    end

    # Bearer owners are already required to be active. A pending session user
    # gets 403 so the SPA can show the holding page.
    def require_active_account!
      return unless session_authenticated?
      return if console_session_user.active?

      render_error(status: :forbidden, message: "account pending approval", details: { reason: "account_pending" })
    end

    def verify_session_csrf_token!
      return unless session_authenticated?
      return if CSRF_SAFE_METHODS.include?(request.request_method)
      return if valid_request_origin? && any_authenticity_token_valid?

      render_error(status: :forbidden, message: "invalid or missing CSRF token", details: { reason: "csrf" })
    end

    def require_admin!
      return if acting_admin?

      message = api_key_authenticated? ? "API key owner is not an admin" : "admin access required"
      render_error(status: :forbidden, message: message, details: { reason: "admin_required" })
    end

    # Records an active-user endpoint may expose: everything for an acting
    # admin, otherwise only rows whose +owner+ association/column is the caller.
    def owned_scope(scope, owner: :user)
      acting_admin? ? scope : scope.where(owner => current_user)
    end

    # Guard for a single record loaded by an active-user endpoint. Renders 404
    # (not 403) so record existence is not disclosed to non-owners.
    def require_owner_or_admin!(record, owner: :user)
      return true if acting_admin? || record.public_send(owner) == current_user

      render_error(status: :not_found, message: "not found")
      false
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
