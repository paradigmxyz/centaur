module Api
  module V1
    # Lists the grants attached to a single grantee (a principal or a role):
    #   GET /api/v1/principals/:principal_id/grants
    #   GET /api/v1/roles/:role_id/grants
    #
    # The grantee is resolved by oid first, so an unknown grantee returns 404
    # rather than an empty page. Grant payloads match GrantsController#show.
    class GranteeGrantsController < Api::BaseController
      def index
        grantee = resolve_grantee
        scope = grantee.grants.order(created_at: :asc, id: :asc)

        limit = pagination_limit
        page = pagination_page
        total = scope.count
        records = scope.limit(limit).offset((page - 1) * limit)
        total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)

        render json: {
          data: records.map { |g| grant_payload(g) },
          meta: { page: page, limit: limit, total: total, total_pages: total_pages }
        }
      end

      private

      def resolve_grantee
        if params[:principal_id]
          Principal.find_by_oid!(params[:principal_id])
        else
          Role.find_by_oid!(params[:role_id])
        end
      end

      def grant_payload(grant)
        grantee = grant.grantee
        grantable = grant.grantable
        {
          id: grant.oid,
          GrantsController::GRANTEE_TYPES.key(grantee.class) => grantee.oid,
          GrantsController::GRANTABLE_TYPES.key(grantable.class) => grantable.oid,
          created_at: grant.created_at,
          updated_at: grant.updated_at
        }
      end
    end
  end
end
