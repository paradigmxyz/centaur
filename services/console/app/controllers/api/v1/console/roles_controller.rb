module Api
  module V1
    module Console
      # Roles for the console SPA (session only). The list adds two filters on
      # top of labels and pagination: q (name or foreign_id, case-insensitive)
      # and exclude_principal (a principal oid; omits the roles already
      # assigned to it, for the assign-role picker; 404 when unknown). Slack
      # channel permissions can be replaced as a set or removed one channel at
      # a time.
      class RolesController < ::Api::V1::RolesController
        include ConsoleApiAuthentication
        include SlackChannelPermissionReplacement

        def index
          records, meta = paginated_label_search(filtered_roles(Role.includes(:slack_channel_permissions)))
          render json: { data: records.map { |r| record_payload(r) }, meta: meta }
        end

        private

        def filtered_roles(scope)
          if (oid = params[:exclude_principal].presence)
            principal = Principal.find_by_oid!(oid.to_s)
            scope = scope.where.not(id: principal.principal_roles.select(:role_id))
          end
          query = params[:q].to_s.strip
          if query.present?
            pattern = "%#{Role.sanitize_sql_like(query)}%"
            scope = scope.where("roles.name ILIKE :pattern OR roles.foreign_id ILIKE :pattern", pattern: pattern)
          end
          scope
        end
      end
    end
  end
end
