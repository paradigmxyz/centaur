module Api
  module V1
    # Operator (console user) management for the console Users page:
    #
    #   GET  /api/v1/users              every console user (unpaginated)
    #   POST /api/v1/users/:id/approve  activate a pending user, recording the approver
    #   POST /api/v1/users/:id/disable  revoke access (not your own account)
    #   POST /api/v1/users/:id/promote  make an active admin
    #   POST /api/v1/users/:id/demote   drop admin (not your own account)
    #
    # Admin-only. Disabling also revokes the user's MCP OAuth refresh tokens,
    # and their console session is reset on its next request.
    class UsersController < Api::BaseController
      accepts_credentials :session

      STATUSES = User.statuses.keys.freeze

      before_action :set_user, except: :index

      def index
        scope = User.includes(:user_identities, :approved_by)
        if params[:status].present?
          raise ActionController::BadRequest, "status must be one of #{STATUSES.join(', ')}" unless STATUSES.include?(params[:status])

          scope = scope.where(status: params[:status])
        end
        users = scope.order(admin: :desc, email: :asc, id: :asc).to_a
        render json: { data: users.map { |user| user_payload(user) }, meta: { total: users.size } }
      end

      def approve
        @user.approve!(by: current_user)
        render_user
      end

      def disable
        return render_self_error("You can't disable your own account.") if @user == current_user

        @user.update!(status: :disabled)
        render_user
      end

      # Promoting also activates: an admin must be able to use the console.
      def promote
        @user.update!(admin: true, status: :active)
        render_user
      end

      def demote
        return render_self_error("You can't demote your own account.") if @user == current_user

        @user.update!(admin: false)
        render_user
      end

      private

      def set_user
        @user = User.find_by_oid!(params[:id])
      end

      def render_user
        render json: { data: user_payload(@user.reload) }
      end

      def render_self_error(message)
        render_error(status: :unprocessable_entity, message: message, details: { reason: "self" })
      end

      def user_payload(user)
        {
          id: user.oid,
          email: user.email,
          name: user.name,
          status: user.status,
          admin: user.admin?,
          current: user == current_user,
          identity_providers: user.user_identities.map(&:provider).uniq.sort,
          approved_at: user.approved_at,
          approved_by: user.approved_by && { id: user.approved_by.oid, email: user.approved_by.email },
          created_at: user.created_at,
          updated_at: user.updated_at
        }
      end
    end
  end
end
