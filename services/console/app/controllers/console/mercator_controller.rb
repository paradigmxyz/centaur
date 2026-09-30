module Console
  class MercatorController < ApplicationController
    layout "console"
    before_action :require_admin
    class_attribute :connection_client_factory, default: -> { Mercator::ConnectionClient.new }

    def show
      @credential = Mercator::Connection.credential
      @manual_credentials = Mercator::Connection.manual_configuration?
      return unless @credential && !@credential.dead?
      @credential.refresh! if @credential.expires_at && @credential.expires_at <= Time.current
      @status = connection_client_factory.call.status(@credential.access_token) unless @credential.dead?
    rescue Broker::ExchangeError => error
      @authorization_failed = [ 401, 403 ].include?(error.status)
    rescue Broker::RefreshError
      @authorization_failed = true
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
