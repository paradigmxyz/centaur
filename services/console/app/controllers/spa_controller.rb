# Serves the built console SPA shell for browser navigations matched by
# SpaRequest. The build puts content-hashed assets in public/spa/assets and the
# shell in spa-shell/index.html, outside public/, so the shell is only ever
# served from here, with the Content-Security-Policy header (static files under
# public/ get none). Authentication happens in the
# SPA through GET /api/v1/me, so this controller is deliberately public: the
# HTML shell holds no data.
class SpaController < ActionController::Base
  # Same browser floor as the server-rendered pages (ApplicationController).
  allow_browser versions: :modern

  include ConsoleSignIn

  class_attribute :index_path, default: Rails.root.join("spa-shell", "index.html")

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

  # A signed-out deep link ("/" or a /console page) is remembered so the
  # browser lands back on it after signing in. Only allowlisted paths
  # (ConsoleReturnPath) are stored; the SPA's own auth pages never are.
  def remember_return_path
    return if session[:user_id]

    remember_console_return_path(request.fullpath)
  end
end
