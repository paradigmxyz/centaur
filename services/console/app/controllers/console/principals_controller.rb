module Console
  # Mutations for the principal detail page: assign/unassign roles and grant/revoke
  # secrets directly. The read-only show action lives on ConsoleController; this
  # controller only handles the POST/DELETE actions wired from that page. Gated by
  # the app-wide require_login (not admin -- mirrors the secret/credential forms).
  class PrincipalsController < ApplicationController
    include KvRowParams
    include SecretKinds

    layout "console"

    before_action :require_admin
    before_action :set_principal, except: %i[new create]

    def new
      @principal = Principal.new(namespace: "default")
    end

    def create
      @principal = Principal.new(created_by: current_user)
      assign_form(@principal)
      if @principal.save
        redirect_to console_principal_path(@principal.oid), notice: "Principal created."
      else
        render :new, status: :unprocessable_entity
      end
    end

    def destroy
      label = principal_label(@principal)
      @principal.destroy!
      redirect_to console_principals_path, notice: "Deleted principal #{label}."
    end

    def update_sandbox_access
      @principal.update!(
        sandbox_repo_cache: params[:sandbox_repo_cache],
        sandbox_observability_enabled: ActiveModel::Type::Boolean.new.cast(params[:sandbox_observability_enabled]),
        sandbox_api_server_enabled: ActiveModel::Type::Boolean.new.cast(params[:sandbox_api_server_enabled])
      )
      redirect_to console_principal_path(@principal.oid), notice: "Updated sandbox access."
    rescue ActiveRecord::RecordInvalid => e
      redirect_to console_principal_path(@principal.oid), alert: e.record.errors.full_messages.to_sentence
    end

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

    def assign_form(principal)
      fields = principal_params.permit(:namespace, :foreign_id, :name)
      fields[:namespace] = fields[:namespace].presence || "default"
      fields[:foreign_id] = fields[:foreign_id].presence
      principal.assign_attributes(fields)
      principal.labels = label_params
    end

    def principal_params
      params.fetch(:principal, ActionController::Parameters.new)
    end

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

    def principal_label(principal)
      principal.name.presence || principal.foreign_id.presence || principal.oid
    end

    def set_principal
      @principal = Principal.find_by_oid!(params[:id])
    end
  end
end
