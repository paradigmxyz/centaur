module Api
  module V1
    module Console
      # GET /api/v1/console/secrets
      #
      # Grantable secrets of every kind in one list (for grant pickers and the
      # console secrets table). Only identifying metadata is returned, plus the
      # source backend labels and the managing OAuth integration -- never source
      # references or values. Ordered by name (case-insensitive), then creation.
      #
      # Query params: q (name or foreign_id, case-insensitive), kind (one of the
      # secret kind slugs), exclude_grantee ("principal:<oid>" or "role:<oid>";
      # omits secrets granted directly to that grantee), page, limit.
      #
      # GET /api/v1/console/secrets/:kind/:id
      #
      # The console detail view of one secret of any kind: identity, the managing
      # integration, source backends and references (an inline value is shown as
      # "inline"), injection target, configuration rows, signed headers, and
      # request rules. Unknown kinds and ids are 404s.
      class SecretsController < BaseController
        def index
          kinds = selected_kinds
          query = params[:q].to_s.strip
          granted = granted_secret_ids
          rows = kinds.flat_map do |kind, config|
            model = config.fetch(:model)
            scope = model.includes(config.fetch(:includes))
            scope = scope.includes(broker_credential: :oauth_app) if model == StaticSecret
            excluded = granted&.fetch(:"#{kind}_secret_id", nil)
            scope = scope.where.not(id: excluded) if excluded.present?
            if query.present?
              pattern = "%#{model.sanitize_sql_like(query)}%"
              scope = scope.where("name ILIKE :pattern OR foreign_id ILIKE :pattern", pattern: pattern)
            end
            scope.to_a
          end
          rows.sort_by! { |secret| [ secret.name.to_s.downcase, secret.created_at, secret.id ] }

          limit = pagination_limit
          page = pagination_page
          total = rows.size
          page_rows = rows.slice((page - 1) * limit, limit) || []
          total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)
          render json: {
            data: page_rows.map { |secret| secret_payload(secret) },
            meta: { page: page, limit: limit, total: total, total_pages: total_pages }
          }
        end

        def show
          secret = SecretKinds.find_secret!(params[:kind], params[:id])
          render json: { data: detail_payload(secret) }
        end

        private

        def selected_kinds
          kind = params[:kind].presence
          return SecretKinds::SECRET_KINDS unless kind
          raise ActionController::BadRequest, "kind must be one of #{SecretKinds::SECRET_KINDS.keys.join(", ")}" unless SecretKinds::SECRET_KINDS.key?(kind)

          SecretKinds::SECRET_KINDS.slice(kind)
        end

        # Secret ids already granted to the exclude_grantee, keyed by grant column,
        # or nil when no grantee filter was given.
        def granted_secret_ids
          grantee = principal_or_role_param!(:exclude_grantee)
          return unless grantee

          columns = Grant::GRANTABLE_ASSOCIATIONS.map { |assoc| :"#{assoc}_id" }
          grantee.grants.pluck(*columns).each_with_object(Hash.new { |h, k| h[k] = [] }) do |row, ids|
            columns.zip(row).each { |column, id| ids[column] << id if id }
          end
        end

        def secret_payload(secret)
          GrantableSummary.call(secret).merge(
            grant_key: :"#{GrantableSummary.kind_for(secret)}_secret_id",
            description: secret.description,
            labels: secret.labels,
            source_types: SecretConsoleView.source_types(secret),
            managed_by: SecretConsoleView.managed_by(secret),
            created_at: secret.created_at,
            updated_at: secret.updated_at
          )
        end

        def detail_payload(secret)
          kind = GrantableSummary.kind_for(secret)
          secret_payload(secret).merge(
            editable: SecretKinds::SECRET_KINDS.dig(kind, :form) == true,
            injection: SecretConsoleView.injection(secret),
            source_segments: SecretConsoleView.source_segments(secret),
            configuration: SecretConsoleView.configuration(secret),
            signed_headers: SecretConsoleView.signed_headers(secret),
            rules: SecretConsoleView.rules(secret)
          )
        end
      end
    end
  end
end
