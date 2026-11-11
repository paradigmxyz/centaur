# Serves the PWA web app manifest and service worker. Both are static files
# under config/pwa (no templates, no session): the browser fetches them outside
# an authenticated page load. They are served with no-cache rather than from
# public/ so a deploy's changes reach installed apps without waiting out the
# long-lived public file cache headers. The SPA registers the worker
# (web/src/features/auth-pages/pwa.ts).
class PwaController < ActionController::API
  FILES_DIR = Rails.root.join("config", "pwa")

  def manifest
    serve "manifest.json", "application/manifest+json"
  end

  def service_worker
    # Scope "/" needs no Service-Worker-Allowed header: the script lives at /.
    serve "service-worker.js", "text/javascript"
  end

  private

  def serve(name, type)
    response.headers["Cache-Control"] = "no-cache"
    send_file FILES_DIR.join(name), type: type, disposition: "inline"
  end
end
