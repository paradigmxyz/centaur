module Api
  module V1
    module Console
      # Principals for the console SPA (session only).
      #
      # The list and show payloads add the identity fields and roles the
      # console pages show (kind, slack_user_id, roles,
      # inherited_slack_channel_permissions); the list takes q (name or
      # foreign_id, case-insensitive) and sort=name (unnamed last). The console
      # also deletes principals, reads effective grants, and replaces or
      # removes Slack channel permissions.
      class PrincipalsController < ::Api::V1::PrincipalsController
        include ConsoleApiAuthentication
        include PrincipalEffectiveGrants
        include SlackChannelPermissionReplacement

        def index
          scope = Principal.includes(:console_user, :slack_channel_permissions, roles: :slack_channel_permissions)
          query = params[:q].to_s.strip
          if query.present?
            pattern = "%#{Principal.sanitize_sql_like(query)}%"
            scope = scope.where("principals.name ILIKE :pattern OR principals.foreign_id ILIKE :pattern", pattern: pattern)
          end
          records, meta = paginated_label_search(scope, order: index_order)
          render json: { data: records.map { |p| console_record_payload(p) }, meta: meta }
        end

        def show
          render json: { data: console_record_payload(Principal.find_by_oid!(params[:id])) }
        end

        # DELETE /api/v1/console/principals/:id
        #
        # Removes the principal with its direct grants, role assignments, Slack
        # channel permissions, scheduled tasks, MCP OAuth codes/tokens, and sync
        # snapshots; assigned proxies are unassigned.
        def destroy
          Principal.find_by_oid!(params[:id]).destroy!
          head :no_content
        end

        private

        def index_order
          case params[:sort].presence
          when nil then { created_at: :asc, id: :asc }
          when "name" then [ Arel.sql("principals.name ASC NULLS LAST"), { created_at: :asc, id: :asc } ]
          else raise ActionController::BadRequest, "sort must be name"
          end
        end

        def effective_grant_payload(kind, secret, sources)
          super.merge(kind: kind)
        end

        def console_record_payload(principal)
          record_payload(principal).merge(
            kind: principal.kind,
            slack_user_id: principal.slack_user_id,
            roles: principal.roles.sort_by(&:id).map { |role| { id: role.oid, foreign_id: role.foreign_id, name: role.name } },
            inherited_slack_channel_permissions: principal.inherited_slack_channel_permissions_payload
          )
        end
      end
    end
  end
end
