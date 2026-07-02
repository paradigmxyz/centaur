module Console
  # Shared skeleton for the per-type secret form controllers: the new/create/edit/
  # update actions, view resolution, and the request mapping every secret shares
  # (identity + labels, and building its SecretSource). A subclass declares its
  # #model and #kind and implements #assign_form for the type-specific config,
  # source association, and (where applicable) rules.
  class BaseSecretsController < ApplicationController
    include SecretKinds
    include KvRowParams

    layout "console"

    before_action :require_admin
    before_action :assign_kind
    before_action :set_secret, only: %i[edit update destroy]

    def new
      @secret = model.new(namespace: "default")
    end

    def create
      @secret = model.new(created_by: current_user)
      assign_form(@secret)
      if @secret.save
        redirect_to console_secret_path(kind, @secret.oid), notice: "Secret created."
      else
        render :new, status: :unprocessable_entity
      end
    end

    def edit; end

    def update
      assign_form(@secret)
      if @secret.save
        redirect_to console_secret_path(kind, @secret.oid), notice: "Secret updated."
      else
        render :edit, status: :unprocessable_entity
      end
    end

    # Destroys the secret and its dependents (source, rules, and any grants of it,
    # via dependent: :destroy). A managed wrapper can be deleted here too; the OAuth
    # flow recreates it on the next consent.
    def destroy
      if @secret.destroy
        redirect_to console_secrets_path, notice: "Secret deleted."
      else
        redirect_to console_secret_path(kind, @secret.oid),
                    alert: @secret.errors.full_messages.to_sentence.presence || "Could not delete secret."
      end
    end

    private

    def model
      raise NotImplementedError, "#{self.class} must define #model"
    end

    def kind
      raise NotImplementedError, "#{self.class} must define #kind"
    end

    def assign_form(_secret)
      raise NotImplementedError, "#{self.class} must define #assign_form"
    end

    # namespace / foreign_id / name / description / labels. A blank namespace
    # defaults to "default"; a blank foreign_id becomes nil so the allow_nil
    # validations apply (an empty string would fail the URL-safe format).
    def assign_identity(secret)
      fields = params.fetch(:secret, ActionController::Parameters.new)
               .permit(:namespace, :foreign_id, :name, :description)
      fields[:namespace] = fields[:namespace].presence || "default"
      fields[:foreign_id] = fields[:foreign_id].presence
      secret.assign_attributes(fields)
      secret.labels = label_params
    end

    # The SecretSource described by the `source` params, or nil when no backend was
    # chosen. The subclass assigns it to its has_one association (source/dsn_source).
    def build_source
      sp = params.fetch(:source, ActionController::Parameters.new)
      type = sp[:source_type].presence
      return nil if type.nil?

      config = {}
      attrs = { source_type: type }
      if type == "control_plane"
        attrs[:secret] = sp[:secret]
      elsif (ref_key = SOURCE_REF_KEYS[type])
        config[ref_key] = sp[:reference].strip if sp[:reference].present?
      end
      config["region"] = sp[:region].strip if sp[:region].present? && %w[aws_sm aws_ssm].include?(type)
      config["json_key"] = sp[:json_key].strip if sp[:json_key].present?
      attrs[:config] = config

      SecretSource.new(attrs)
    end

    def assign_kind
      @kind = kind
    end

    def set_secret
      @secret = model.find_by_oid!(params[:id])
    end
  end
end
