module Console
  # Create/edit form for OAuth apps: identity, the provider + OAuth client it
  # consents with, the flow policy (allowed scopes, allowed return URLs, the
  # credential namespace, the enable kill switch), and labels. Modeled on
  # Console::BrokerCredentialsController -- an OAuth app is operator config, not a
  # secret, so it lives on its own rather than under BaseSecretsController.
  class OauthAppsController < ApplicationController
    include KvRowParams

    layout "console"

    before_action :set_app, only: %i[edit update]

    def new
      @app = OauthApp.new(provider: Oauth::Providers.keys.first, enabled: true)
    end

    def create
      @app = OauthApp.new(created_by: current_user)
      assign_form(@app)
      if @app.save
        redirect_to console_oauth_app_path(@app.oid), notice: "OAuth app created."
      else
        render :new, status: :unprocessable_entity
      end
    end

    def edit; end

    def update
      assign_form(@app)
      if @app.save
        redirect_to console_oauth_app_path(@app.oid), notice: "OAuth app updated."
      else
        render :edit, status: :unprocessable_entity
      end
    end

    private

    # Map the form params onto the app. client_secret is write-only: it is only
    # assigned when non-blank, so editing without re-entering it leaves the stored
    # value in place (same pattern as BrokerCredentialsController).
    def assign_form(app)
      fields = app_params.permit(:slug, :description, :provider, :client_id, :credential_namespace)
      app.assign_attributes(fields)
      app.enabled = app_params[:enabled] == "1"
      app.allowed_scopes = line_list(app_params[:allowed_scopes])
      app.labels = label_params

      secret = app_params[:client_secret]
      app.client_secret = secret if secret.present?
    end

    def app_params
      params.fetch(:oauth_app, ActionController::Parameters.new)
    end

    # Scopes are entered one per line (they contain no spaces); blank lines
    # dropped.
    def line_list(raw)
      raw.to_s.split(/\r?\n/).map(&:strip).reject(&:blank?)
    end

    def set_app
      @app = OauthApp.find_by_oid!(params[:id])
    end
  end
end
