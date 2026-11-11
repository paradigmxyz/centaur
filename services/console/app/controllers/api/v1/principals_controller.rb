module Api
  module V1
    class PrincipalsController < Api::BaseController
      include SlackChannelPermissionApi

      # Optional (additive) query params used by the console: `q` matches name
      # or foreign_id case-insensitively; `sort=name` orders by name (unnamed
      # last) then creation time instead of the default creation order.
      def index
        scope = Principal.includes(:console_user, :slack_channel_permissions, roles: :slack_channel_permissions)
        query = params[:q].to_s.strip
        if query.present?
          pattern = "%#{Principal.sanitize_sql_like(query)}%"
          scope = scope.where("principals.name ILIKE :pattern OR principals.foreign_id ILIKE :pattern", pattern: pattern)
        end
        records, meta = paginated_label_search(scope, order: index_order)
        render json: { data: records.map { |p| record_payload(p) }, meta: meta }
      end

      # GET /api/v1/principals/:id
      #
      # :id is an opaque oid. To read by foreign_id, use the lookup route.
      def show
        principal = Principal.find_by_oid!(params[:id])
        render json: { data: record_payload(principal) }
      end

      # GET /api/v1/principals/lookup/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(Principal)) }
      end

      def create
        principal = Principal.new(foreign_id: data_params[:foreign_id], created_by: current_user)
        ActiveRecord::Base.transaction do
          principal.assign_attributes(principal_params)
          principal.link_console_user_by_slack_email if data_params.key?(:slack_email)
          principal.apply_default_sandbox_capabilities!(principal_params)
          principal.save!
          replace_slack_channel_permissions!(principal) if data_params.key?(:slack_channel_permissions)
        end
        render status: :created, json: { data: record_payload(principal) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent. foreign_id is immutable.
      def update
        principal, was_new = upsert_with_retry
        render status: (was_new ? :created : :ok), json: { data: record_payload(principal) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # DELETE /api/v1/principals/:id
      #
      # Removes the principal with its direct grants, role assignments, Slack
      # channel permissions, scheduled tasks, MCP OAuth codes/tokens, and sync
      # snapshots; assigned proxies are unassigned.
      def destroy
        Principal.find_by_oid!(params[:id]).destroy!
        head :no_content
      end

      # GET /api/v1/principals/:id/effective_grants
      #
      # Secrets granted directly or through roles, with how each one is
      # reached. Paginated (page/limit) like other lists.
      def effective_grants
        principal = Principal.find_by_oid!(params[:id])
        limit = pagination_limit
        result = PrincipalEffectiveGrantsPage.new(
          principal: principal,
          relations: principal.granted_secrets_by_kind,
          page: pagination_page,
          per_page: limit
        ).call

        data = result.records_by_kind.flat_map do |kind, secrets|
          secrets.map do |secret|
            effective_grant_payload(kind, secret, result.sources_by_key[[ kind, secret.id ]])
          end
        end
        total_pages = result.total_count.zero? ? 0 : result.total_pages
        render json: { data: data, meta: { page: result.page, limit: limit,
                                           total: result.total_count, total_pages: total_pages } }
      end

      # GET /api/v1/principals/:id/effective_config
      # GET /api/v1/principals/lookup/:foreign_id/effective_config
      #
      # Addressable by opaque oid (member route) or by foreign_id (lookup route).
      #
      # The config this principal resolves to, in the same shape iron-proxy
      # receives on /sync, for operator inspection. Unlike /sync it never reveals
      # live secrets (inline control_plane values are redacted) and does no
      # config-hash negotiation. We send a content-derived ETag for change
      # detection but mark the response no-store, since it reflects mutable
      # grants and must never be served from a cache.
      def effective_config
        principal = params[:foreign_id].present? ? find_by_foreign_id!(Principal) : Principal.find_by_oid!(params[:id])
        body = { data: { id: principal.oid }.merge(PrincipalSyncConfig.redacted_config_for(principal)) }.to_json

        response.headers["ETag"] = %("#{Digest::SHA256.hexdigest(body)}")
        response.headers["Cache-Control"] = "no-store"
        render json: body
      end

      private

      # Concurrent first turns upsert the same new foreign_id. The loser fails
      # the unique index or the uniqueness validation, so retry once, which
      # finds and updates the winner's record.
      def upsert_with_retry
        upsert!
      rescue ActiveRecord::RecordNotUnique
        upsert!
      rescue ActiveRecord::RecordInvalid => e
        raise unless e.record.errors.of_kind?(:foreign_id, :taken)

        upsert!
      end

      def upsert!
        principal = resolve_for_upsert(Principal)
        was_new = principal.new_record?
        ActiveRecord::Base.transaction do
          principal.assign_attributes(principal_params)
          principal.link_console_user_by_slack_email if data_params.key?(:slack_email)
          principal.apply_default_sandbox_capabilities!(principal_params) if was_new
          principal.save!
          replace_slack_channel_permissions!(principal) if data_params.key?(:slack_channel_permissions)
        end
        [ principal, was_new ]
      end

      def index_order
        case params[:sort].presence
        when nil then { created_at: :asc, id: :asc }
        when "name" then [ Arel.sql("principals.name ASC NULLS LAST"), { created_at: :asc, id: :asc } ]
        else raise ActionController::BadRequest, "sort must be name"
        end
      end

      def effective_grant_payload(kind, secret, sources)
        roles = sources.filter_map { |source| source[:role] }.uniq
        {
          id: secret.oid,
          kind: kind,
          grant_key: PrincipalEffectiveGrantsPage::GRANT_COLUMN_BY_KIND.fetch(kind),
          foreign_id: secret.foreign_id,
          name: secret.try(:name),
          direct: sources.any? { |source| source[:type] == :direct },
          roles: roles.map { |role| { id: role.oid, foreign_id: role.foreign_id, name: role.name } }
        }
      end

      def record_payload(principal)
        {
          id: principal.oid,
          foreign_id: principal.foreign_id,
          name: principal.name,
          labels: principal.labels_with_sandbox_capabilities,
          slack_channel_permissions: principal.slack_channel_permissions_payload,
          effective_slack_channel_permissions: principal.effective_slack_channel_permissions_payload,
          sandbox_repo_cache: principal.sandbox_repo_cache,
          sandbox_observability_enabled: principal.sandbox_observability_enabled,
          sandbox_sessions_read_enabled: principal.sandbox_sessions_read_enabled,
          sandbox_workflows_read_enabled: principal.sandbox_workflows_read_enabled,
          sandbox_workflows_write_enabled: principal.sandbox_workflows_write_enabled,
          created_at: principal.created_at,
          updated_at: principal.updated_at
        }.merge(console_view? ? console_payload(principal) : {})
      end

      # `view=console` (index/show) adds what the console pages display. The
      # default payload deliberately omits first-class identity fields.
      def console_view?
        params[:view] == "console"
      end

      def console_payload(principal)
        {
          kind: principal.kind,
          slack_user_id: principal.slack_user_id,
          roles: principal.roles.sort_by(&:id).map { |role| { id: role.oid, foreign_id: role.foreign_id, name: role.name } },
          inherited_slack_channel_permissions: principal.inherited_slack_channel_permissions_payload
        }
      end

      def principal_params
        data_params.permit(
          :name,
          :kind,
          :slack_user_id,
          :slack_channel_id,
          :slack_team_id,
          :slack_email,
          :console_user_id,
          :sandbox_repo_cache,
          :sandbox_observability_enabled,
          :sandbox_sessions_read_enabled,
          :sandbox_workflows_read_enabled,
          :sandbox_workflows_write_enabled,
          labels: {}
        )
      end

      def slack_channel_permission_owner
        Principal.find_by_oid!(params[:id])
      end
    end
  end
end
