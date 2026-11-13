# Content-Security-Policy for the server-rendered sign-in and OAuth pages
# (layouts/oauth: login, pending, MCP consent, broker OAuth result). They have
# no JavaScript and only the plain CSS, font, and images in public/assets/oauth-page,
# so they narrow the app-wide policy (config/initializers/content_security_policy.rb)
# to no script and no inline style. It is applied ahead of every other
# callback so a page rendered or redirected by an earlier before_action (the
# login gate, an unknown integration) carries it too.
#
# Deliberately no form-action: the MCP consent form (POST /mcp/oauth/authorize)
# redirects to the client's registered redirect_uri (http://127.0.0.1:<port>,
# cursor://, vscode://, chrome-extension://, https://...), and browsers apply
# form-action to the redirect that follows a submission. The login form's
# redirect can end there too (POST /login -> /mcp/oauth/authorize -> an error
# redirect to the client), so form-action 'self' is unsafe on every page of
# this flow.
module ServerRenderedPagePolicy
  extend ActiveSupport::Concern

  included do
    prepend_before_action :apply_server_rendered_page_policy
  end

  private

  def apply_server_rendered_page_policy
    policy = current_content_security_policy
    policy.script_src :none
    policy.style_src :self
    policy.style_src_elem # removed: falls back to style-src 'self'
    request.content_security_policy = policy
  end
end
