module Console
  # Operator UI for roles: list/detail/create/edit and role-scoped secret grants.
  # Roles are namespace-scoped bundles of secrets that principals can inherit.
  class RolesController < ApplicationController
    include KvRowParams
    include SecretKinds

    layout "console"

    before_action :set_role, only: %i[show edit update grant_secret revoke_grant]

    def index
      @roles = Role.order(:namespace, :id)
    end

    def show
      @grants = @role.grants.includes(Grant::GRANTABLE_ASSOCIATIONS).order(:id)
      granted_ids = Hash.new { |h, k| h[k] = [] }
      @grants.each do |grant|
        assoc = grantable_assoc_for(grant)
        next unless assoc
        granted_ids[assoc.to_s.delete_suffix("_secret")] << grant.public_send("#{assoc}_id")
      end
      @assignable_secrets = SECRET_KINDS.each_with_object({}) do |(kind, cfg), acc|
        acc[kind] = cfg[:model]
          .where(namespace: @role.namespace)
          .where.not(id: granted_ids[kind])
          .order(:id)
      end
    end

    def new
      @role = Role.new(namespace: "default")
    end

    def create
      @role = Role.new(created_by: current_user)
      assign_form(@role, include_readonly: true)
      if @role.save
        redirect_to console_role_path(@role.oid), notice: "Role created."
      else
        render :new, status: :unprocessable_entity
      end
    end

    def edit; end

    def update
      assign_form(@role, include_readonly: false)
      if @role.save
        redirect_to console_role_path(@role.oid), notice: "Role updated."
      else
        render :edit, status: :unprocessable_entity
      end
    end

    def grant_secret
      secret = resolve_grantable(params[:grantable])
      return redirect_to console_role_path(@role.oid), alert: "Pick a secret to grant." unless secret
      unless secret.namespace == @role.namespace
        return redirect_to console_role_path(@role.oid),
                           alert: "Secret must be in the same namespace as the role."
      end

      @role.grants.create_with(created_by: current_user)
           .find_or_create_by!(grantable_assoc(secret) => secret)
      redirect_to console_role_path(@role.oid), notice: "Granted #{secret_label(secret)}."
    rescue ActiveRecord::RecordNotUnique
      # A concurrent submit already created the grant; the requested end state is
      # true, so report success rather than 500.
      redirect_to console_role_path(@role.oid), notice: "Granted #{secret_label(secret)}."
    rescue ActiveRecord::RecordInvalid => e
      redirect_to console_role_path(@role.oid), alert: e.record.errors.full_messages.to_sentence
    end

    def revoke_grant
      grant = @role.grants.find_by_oid!(params[:grant_id])
      grant.destroy!
      redirect_to console_role_path(@role.oid), notice: "Revoked grant."
    end

    private

    def assign_form(role, include_readonly:)
      fields =
        if include_readonly
          role_params.permit(:namespace, :foreign_id, :name)
        else
          role_params.permit(:name)
        end
      if include_readonly
        fields[:namespace] = fields[:namespace].presence || "default"
        fields[:foreign_id] = fields[:foreign_id].presence
      end
      role.assign_attributes(fields)
      role.labels = label_params
    end

    def role_params
      params.fetch(:role, ActionController::Parameters.new)
    end

    def resolve_grantable(value)
      kind, oid = value.to_s.split(":", 2)
      cfg = SECRET_KINDS[kind]
      return nil if cfg.nil? || oid.blank?
      cfg[:model].find_by_oid!(oid)
    end

    def grantable_assoc(secret)
      secret.class.name.underscore.to_sym
    end

    def grantable_assoc_for(grant)
      Grant::GRANTABLE_ASSOCIATIONS.find { |assoc| grant.public_send("#{assoc}_id") }
    end

    def secret_label(secret)
      secret.try(:name).presence || secret.foreign_id.presence || secret.oid
    end

    def set_role
      @role = Role.find_by_oid!(params[:id])
    end
  end
end
