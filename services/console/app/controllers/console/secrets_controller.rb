module Console
  # Mutations for grants from the secret detail page. The read-only show action
  # lives on ConsoleController; this controller only handles granting a displayed
  # secret to roles and revoking those role grants.
  class SecretsController < ApplicationController
    include SecretKinds

    layout "console"

    before_action :set_secret

    def grant_role
      role = Role.find_by_oid!(params[:role_id])
      unless role.namespace == @secret.namespace
        return redirect_to console_secret_path(@kind, @secret.oid),
                           alert: "Role must be in the same namespace as the secret."
      end

      Grant.create_with(created_by: current_user)
           .find_or_create_by!(role: role, grantable_assoc => @secret)
      redirect_to console_secret_path(@kind, @secret.oid),
                  notice: "Assigned secret to #{role_label(role)}."
    rescue ActiveRecord::RecordNotUnique
      # A concurrent submit already created the grant; the end state is what the
      # operator asked for, so report success rather than 500.
      redirect_to console_secret_path(@kind, @secret.oid),
                  notice: "Assigned secret to #{role_label(role)}."
    rescue ActiveRecord::RecordInvalid => e
      redirect_to console_secret_path(@kind, @secret.oid), alert: e.record.errors.full_messages.to_sentence
    end

    def revoke_role_grant
      grant = Grant.where(grantable_assoc => @secret)
                   .where.not(role_id: nil)
                   .find_by_oid!(params[:grant_id])
      grant.destroy!
      redirect_to console_secret_path(@kind, @secret.oid), notice: "Unassigned secret from role."
    end

    private

    def set_secret
      @kind = params[:kind]
      cfg = SECRET_KINDS[@kind]
      return render plain: "secret not found", status: :not_found unless cfg
      @secret = cfg[:model].find_by_oid!(params[:id])
    end

    def grantable_assoc
      @secret.class.name.underscore.to_sym
    end

    def role_label(role)
      role.name.presence || role.foreign_id.presence || role.oid
    end
  end
end
