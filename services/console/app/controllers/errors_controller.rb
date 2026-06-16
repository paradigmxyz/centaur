class ErrorsController < ActionController::API
  def not_found
    render status: :not_found, json: { error: { message: "not found" } }
  end
end
