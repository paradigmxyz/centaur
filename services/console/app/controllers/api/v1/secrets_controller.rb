module Api
  module V1
    # GET /api/v1/secrets
    #
    # Grantable secrets of every kind in one list (for grant pickers and the
    # console secrets table). Only identifying metadata is returned, never
    # sources or values. Ordered by name (case-insensitive), then creation.
    #
    # Query params: q (name or foreign_id, case-insensitive), kind (one of the
    # secret kind slugs), page, limit.
    class SecretsController < Api::BaseController
      def index
        kinds = selected_kinds
        query = params[:q].to_s.strip
        rows = kinds.flat_map do |_kind, config|
          scope = config.fetch(:model).all
          if query.present?
            pattern = "%#{config.fetch(:model).sanitize_sql_like(query)}%"
            scope = scope.where("name ILIKE :pattern OR foreign_id ILIKE :pattern", pattern: pattern)
          end
          scope.to_a
        end
        rows.sort_by! { |secret| [ secret.try(:name).to_s.downcase, secret.created_at, secret.id ] }

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

      private

      def selected_kinds
        kind = params[:kind].presence
        return SecretKinds::SECRET_KINDS unless kind
        raise ActionController::BadRequest, "kind must be one of #{SecretKinds::SECRET_KINDS.keys.join(", ")}" unless SecretKinds::SECRET_KINDS.key?(kind)

        SecretKinds::SECRET_KINDS.slice(kind)
      end

      def secret_payload(secret)
        GrantableSummary.call(secret).merge(
          grant_key: :"#{GrantableSummary.kind_for(secret)}_secret_id",
          description: secret.try(:description),
          labels: secret.labels,
          created_at: secret.created_at,
          updated_at: secret.updated_at
        )
      end
    end
  end
end
