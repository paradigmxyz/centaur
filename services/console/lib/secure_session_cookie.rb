# Marks the console session cookie Secure whenever it can be: on every request
# that arrived over HTTPS (directly or through a TLS-terminating ingress that
# sets X-Forwarded-Proto), and on every request when the console's public URL
# is https. A cookie issued over HTTPS is then never sent back over plain
# HTTP, while deployments that serve the console over plain HTTP (local
# clusters, internal-only installs) keep working.
#
# Runs inside the session middleware so it can adjust the options the session
# store uses when it writes the cookie on the way out.
class SecureSessionCookie
  def initialize(app, always: false)
    @app = app
    @always = always
  end

  def call(env)
    if @always || Rack::Request.new(env).ssl?
      options = env[Rack::RACK_SESSION_OPTIONS]
      options[:secure] = true if options
    end
    @app.call(env)
  end

  def self.https_public_url?(public_url)
    URI.parse(public_url.to_s).scheme == "https"
  rescue URI::InvalidURIError
    false
  end
end
