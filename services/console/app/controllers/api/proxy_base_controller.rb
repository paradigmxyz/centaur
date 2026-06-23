module Api
  # Base controller for endpoints called by iron-proxy instances. Unlike
  # Api::BaseController (which authenticates a user-owned ApiKey), this
  # authenticates a Proxy's bearer token.
  class ProxyBaseController < ActionController::API
    before_action :authenticate_proxy!

    rescue_from ActionController::ParameterMissing, with: :render_bad_request
    rescue_from ActionController::BadRequest, with: :render_bad_request

    attr_reader :current_proxy

    private

    def authenticate_proxy!
      token = bearer_token
      @current_proxy = Proxy.find_by_token(token) if token.present?
      return if @current_proxy

      render_error(status: :unauthorized, message: "invalid or missing proxy token")
    end

    def bearer_token
      header = request.headers["Authorization"].to_s
      return nil unless header.start_with?("Bearer ")
      header.sub(/\ABearer\s+/, "").presence
    end

    def render_error(status:, message:, details: nil)
      body = { error: { message: message } }
      body[:error][:details] = details if details
      render status: status, json: body
    end

    def render_bad_request(e)
      render_error(status: :bad_request, message: e.message)
    end
  end
end
