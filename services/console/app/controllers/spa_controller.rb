# Serves the built console SPA (services/console/web, built into public/spa)
# for browser navigations matched by SpaRequest. Authentication happens in the
# SPA through GET /api/v1/me, so this controller is deliberately public: the
# HTML shell holds no data.
class SpaController < ActionController::Base
  class_attribute :index_path, default: Rails.public_path.join("spa", "index.html")

  NOT_BUILT_MESSAGE = <<~TEXT.freeze
    The console web app has not been built.
    Run `pnpm -C services/console/web build`, or use the Vite dev server
    (`pnpm -C services/console/web dev`) during development.
  TEXT

  def show
    remember_return_path
    # index.html references content-hashed assets; never cache the shell itself.
    response.headers["Cache-Control"] = "no-cache"

    path = Pathname(index_path)
    return render(plain: NOT_BUILT_MESSAGE, status: :service_unavailable) unless path.file?

    send_file path, type: "text/html; charset=utf-8", disposition: "inline"
  end

  private

  # Mirrors ApplicationController#store_return_to_for_login so a signed-out
  # deep link lands back on the same page after the login redirect.
  def remember_return_path
    return if session[:user_id]
    return unless request.path == "/" || request.path.start_with?("/console")

    session[:return_to] = request.fullpath
  end
end
