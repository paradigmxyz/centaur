module Console
  # Create/edit form for OAuth apps: identity, the provider + OAuth client it
  # consents with, the flow policy (allowed scopes, allowed return URLs, the
  # the enable kill switch, and labels. Modeled on
  # Console::BrokerCredentialsController -- an OAuth app is operator config, not a
  # secret, so it lives on its own rather than under BaseSecretsController.
  class OauthAppsController < ApplicationController
    include KvRowParams

    layout "console"

    class_attribute :registration_client_factory, default: -> { Oauth::DynamicRegistration.new }

    before_action :require_admin
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

    def preset
      provider = Oauth::Providers.fetch(params[:provider])
      unless provider&.respond_to?(:preset) && provider.respond_to?(:registration_endpoint)
        return head :not_found
      end

      preset = provider.preset
      app = OauthApp.find_by(slug: preset.fetch(:slug))
      unless app
        registration = registration_client_factory.call.register(
          endpoint: provider.registration_endpoint,
          redirect_uri: oauth_callback_redirect_uri(preset.fetch(:slug)),
          scope: preset.fetch(:allowed_scopes).join(" "),
          auth_method: provider.respond_to?(:token_endpoint_auth_method) ? provider.token_endpoint_auth_method : "client_secret_post"
        )
        begin
          app = OauthApp.create!(preset.merge(registration).merge(provider: provider.key, enabled: true, created_by: current_user))
        rescue ActiveRecord::RecordNotUnique
          app = OauthApp.find_by!(slug: preset.fetch(:slug))
        rescue ActiveRecord::RecordInvalid => error
          # The uniqueness validator can observe a concurrent winner before INSERT.
          raise unless error.record.errors.of_kind?(:slug, :taken)
          app = OauthApp.find_by!(slug: preset.fetch(:slug))
        end
      end
      unless app.provider == provider.key && app.shared? == preset.fetch(:shared) && app.enabled?
        return redirect_to console_integrations_path, alert: "The preset conflicts with an existing OAuth app. Review its configuration."
      end
      redirect_to oauth_start_path(slug: app.slug)
    rescue Broker::ExchangeError, ActiveRecord::RecordInvalid
      redirect_to console_integrations_path, alert: "Could not set up the integration. Please try again or review the OAuth app configuration."
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
      fields = app_params.permit(:slug, :description, :provider, :client_id)
      app.assign_attributes(fields)
      app.enabled = app_params[:enabled] == "1"
      app.shared = app_params[:shared] == "1" if app.new_record?
      app.always_available = app_params[:always_available] == "1"
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
