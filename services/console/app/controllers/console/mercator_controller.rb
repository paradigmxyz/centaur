module Console
  class MercatorController < ApplicationController
    layout "console"
    before_action :require_admin
    class_attribute :connection_client_factory, default: -> { Mercator::ConnectionClient.new }

    def show
      @credential = Mercator::Connection.credential
      @manual_configuration = Mercator::Connection.manual_configuration? unless @credential
      if @credential && !@credential.dead?
        @claimed = connection_client_factory.call.slack_claimed?(@credential.access_token, wallet: @credential.provider_subject)
        @claim_url = "https://mercator.sh/slack-claim?#{URI.encode_www_form(wallet: @credential.provider_subject)}"
      end
    end

    def connect
      app = Mercator::Connection.prepare!(user: current_user,
        redirect_uri: oauth_callback_redirect_uri(Mercator::Connection::SLUG), client: connection_client_factory.call)
      redirect_to oauth_start_path(slug: app.slug)
    rescue Broker::ExchangeError
      redirect_to console_mercator_path, alert: "Mercator could not start the connection. Please try again or review existing credentials."
    end
  end
end
