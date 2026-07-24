module Console
  # Create/edit form for managed broker credentials: identity, the OAuth client
  # config it refreshes with (token endpoint, client id/secret, scopes, token-
  # endpoint headers), the refresh-policy knobs, and grant-specific write-only
  # initial values. Mirrors the per-type secret form controllers, but a broker
  # credential is not a secret (it is referenced by a token_broker source, not
  # granted), so it lives on its own rather than under BaseSecretsController.
  class BrokerCredentialsController < ApplicationController
    include KvRowParams

    layout "console"

    before_action :require_admin
    before_action :set_credential, only: %i[edit update destroy]

    def new
      @credential = BrokerCredential.new(namespace: "default")
    end

    def create
      @credential = BrokerCredential.new(created_by: current_user)
      assign_form(@credential)
      if @credential.save
        redirect_to console_credential_path(@credential.oid), notice: "Credential created."
      else
        render :new, status: :unprocessable_entity
      end
    end

    def edit; end

    def update
      assign_form(@credential)
      if @credential.save
        redirect_to console_credential_path(@credential.oid), notice: "Credential updated."
      else
        render :edit, status: :unprocessable_entity
      end
    end

    # The model's before_destroy guard throws :abort (adding an error) when a
    # token_broker source still references the credential, so destroy returns
    # false rather than raising; surface that message instead of deleting.
    def destroy
      if @credential.destroy
        redirect_to console_credentials_path, notice: "Credential deleted."
      else
        redirect_to console_credential_path(@credential.oid),
                    alert: @credential.errors.full_messages.to_sentence.presence || "Could not delete credential."
      end
    end

    private

    # Map the form params onto the credential. Encrypted write-only fields are
    # only assigned when non-blank, so editing without re-entering them leaves the
    # stored values in place. Fresh initial values reschedule the credential and
    # mirror the API controller's handling.
    def assign_form(credential)
      fields = credential_params.permit(:namespace, :foreign_id, :name, :description,
                                        :grant, :token_endpoint, :client_id,
                                        :early_refresh_slack_seconds, :early_refresh_fraction,
                                        :max_refresh_interval_seconds, :refresh_timeout_seconds)
      fields[:namespace] = fields[:namespace].presence || "default"
      fields[:foreign_id] = fields[:foreign_id].presence
      credential.assign_attributes(fields)
      credential.scopes = scope_params
      credential.token_endpoint_headers = header_params
      credential.labels = label_params

      secret = credential_params[:client_secret]
      if secret.present?
        credential.client_secret = secret
        reset_refresh_state(credential) if credential.grant == "client_credentials"
      end
      apply_initial_values(credential)
    end

    def apply_initial_values(credential)
      changed = false

      if credential.grant == "preqin"
        preqin_username = credential_params[:preqin_username]
        if preqin_username.present?
          credential.username = preqin_username
          changed = true
        end
      end

      %i[refresh_token username password api_key].each do |field|
        next if field == :username && credential.grant == "preqin"

        value = credential_params[field]
        next if value.blank?

        credential.public_send("#{field}=", value)
        changed = true
      end
      return unless changed

      credential.dead = false
      credential.dead_reason = nil
      credential.failure_count = 0
      credential.next_attempt_at = Time.current
    end

    def reset_refresh_state(credential)
      credential.dead = false
      credential.dead_reason = nil
      credential.failure_count = 0
      credential.next_attempt_at = Time.current
    end

    def credential_params
      params.fetch(:credential, ActionController::Parameters.new)
    end

    # Scopes are entered one per line (whitespace separated); blanks dropped.
    def scope_params
      credential_params[:scopes].to_s.split.map(&:strip).reject(&:blank?)
    end

    # Token-endpoint headers use the same key/value row editor as labels
    # (KvRowParams), but collapse to nil when none are given (the column's
    # default), where labels stay an empty hash.
    def header_params
      kv_rows(params[:headers]).presence
    end

    def set_credential
      @credential = BrokerCredential.find_by_oid!(params[:id])
    end
  end
end
