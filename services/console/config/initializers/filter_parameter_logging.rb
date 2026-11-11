# Be sure to restart your server when you modify this file.

# Configure parameters to be partially matched (e.g. passw matches password) and filtered from the log file.
# Use this to limit dissemination of sensitive information.
# See the ActiveSupport::ParameterFilter documentation for supported notations and behaviors.
Rails.application.config.filter_parameters += [
  :passw, :email, :secret, :token, :_key, :crypt, :salt, :certificate, :otp, :ssn, :cvv, :cvc
]

# OAuth authorization codes and PKCE verifiers (MCP token exchange, consent
# callbacks). Anchored so unrelated "*_code" parameters stay readable.
Rails.application.config.filter_parameters += [ /\Acode\z/i, :code_verifier ]
# "Redirected to …" log lines: the MCP consent decision redirects to the
# client with ?code=<authorization code>.
Rails.application.config.filter_redirect += [ /[?&]code=/ ]
