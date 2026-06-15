module Console
  # Operator (console user) management: approve pending sign-ins, disable access,
  # or promote a user to admin. Admin-only -- the gate is require_admin, layered on
  # top of the app-wide require_login/require_active_account gates.
  class UsersController < ApplicationController
    layout "console"

    before_action :require_admin
    before_action :set_user, only: %i[approve disable promote]

    def index
      @pending = User.pending.includes(:user_identities).order(:created_at)
      @users = User.where.not(status: "pending").includes(:user_identities).order(admin: :desc, email: :asc)
    end

    def approve
      @user.approve!(by: current_user)
      redirect_to console_users_path, notice: "Approved #{@user.email}."
    end

    def disable
      if @user == current_user
        return redirect_to console_users_path, alert: "You can't disable your own account."
      end
      @user.update!(status: :disabled)
      redirect_to console_users_path, notice: "Disabled #{@user.email}."
    end

    def promote
      # Promoting also activates: an admin must be able to use the console.
      @user.update!(admin: true, status: :active)
      redirect_to console_users_path, notice: "#{@user.email} is now an admin."
    end

    private

    def set_user
      @user = User.find_by_oid!(params[:id])
    end
  end
end
