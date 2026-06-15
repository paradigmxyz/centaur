module Console
  # Mutations for the principal detail page: assign/unassign roles and grant/revoke
  # secrets directly. The read-only show action lives on ConsoleController; this
  # controller only handles the POST/DELETE actions wired from that page. Gated by
  # the app-wide require_login (not admin -- mirrors the secret/credential forms).
  class PrincipalsController < ApplicationController
    include SecretKinds

    layout "console"

    before_action :set_principal

    def assign_role
      role = Role.find_by_oid!(params[:role_id])
      @principal.principal_roles.find_or_create_by!(role: role)
      redirect_to console_principal_path(@principal.oid),
                  notice: "Assigned role #{role_label(role)}."
    rescue ActiveRecord::RecordNotUnique
      # A concurrent submit already created the assignment; the end state is what
      # the operator asked for, so report success rather than 500.
      redirect_to console_principal_path(@principal.oid), notice: "Assigned role #{role_label(role)}."
    rescue ActiveRecord::RecordInvalid => e
      redirect_to console_principal_path(@principal.oid), alert: e.record.errors.full_messages.to_sentence
    end

    def unassign_role
      role = Role.find_by_oid!(params[:role_id])
      @principal.principal_roles.find_by!(role: role).destroy!
      redirect_to console_principal_path(@principal.oid),
                  notice: "Unassigned role #{role_label(role)}."
    end

    def grant_secret
      secret = resolve_grantable(params[:grantable])
      return redirect_to console_principal_path(@principal.oid), alert: "Pick a secret to grant." unless secret

      @principal.grants.create_with(created_by: current_user).find_or_create_by!(grantable_assoc(secret) => secret)
      redirect_to console_principal_path(@principal.oid),
                  notice: "Granted #{secret_label(secret)}."
    rescue ActiveRecord::RecordNotUnique
      # A concurrent submit already created the grant; the secret is granted either
      # way, so report success rather than 500.
      redirect_to console_principal_path(@principal.oid), notice: "Granted #{secret_label(secret)}."
    rescue ActiveRecord::RecordInvalid => e
      redirect_to console_principal_path(@principal.oid), alert: e.record.errors.full_messages.to_sentence
    end

    def revoke_grant
      grant = @principal.grants.find_by_oid!(params[:grant_id])
      grant.destroy!
      redirect_to console_principal_path(@principal.oid), notice: "Revoked grant."
    end

    private

    # Parse the "<kind>:<oid>" value from the grant dropdown into a secret record.
    # Returns nil for a blank/unknown selection so the action can flash and bail.
    def resolve_grantable(value)
      kind, oid = value.to_s.split(":", 2)
      cfg = SECRET_KINDS[kind]
      return nil if cfg.nil? || oid.blank?
      cfg[:model].find_by_oid!(oid)
    end

    # The Grant belongs_to association for a grantable record, e.g. a StaticSecret
    # maps to :static_secret. The class name underscores directly to the assoc.
    def grantable_assoc(secret)
      secret.class.name.underscore.to_sym
    end

    def role_label(role)
      role.name.presence || role.foreign_id.presence || role.oid
    end

    def secret_label(secret)
      secret.try(:name).presence || secret.foreign_id.presence || secret.oid
    end

    def set_principal
      @principal = Principal.find_by_oid!(params[:id])
    end
  end
end
