module Api
  # Base controller for endpoints called by iron-proxy instances. Unlike
  # Api::BaseController (which authenticates a user-owned ApiKey), this
  # authenticates a Proxy's bearer token.
  class ProxyBaseController < ActionController::API
    include ApiRequestSupport

    before_action :authenticate_proxy!

    attr_reader :current_proxy

    private

    def authenticate_proxy!
      token = bearer_token
      @current_proxy = Proxy.find_by_token(token) if token.present?
      return if @current_proxy

      render_error(status: :unauthorized, message: "invalid or missing proxy token")
    end
  end
end
