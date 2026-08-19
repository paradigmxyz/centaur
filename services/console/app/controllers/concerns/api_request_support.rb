# Shared plumbing for JSON API controllers: bearer-token extraction and the
# error envelope. Each including controller supplies its own authentication
# scheme on top (user ApiKey, proxy bearer token, sandbox entitlement JWT).
module ApiRequestSupport
  extend ActiveSupport::Concern

  included do
    rescue_from ActionController::ParameterMissing, with: :render_bad_request
    rescue_from ActionController::BadRequest, with: :render_bad_request
  end

  private

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
