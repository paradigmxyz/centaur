# Be sure to restart your server when you modify this file.

# Configure parameters to be partially matched (e.g. passw matches password) and filtered from the log file.
# Use this to limit dissemination of sensitive information.
# See the ActiveSupport::ParameterFilter documentation for supported notations and behaviors.
require Rails.root.join("lib/console_log_filter").to_s

base_filters = [
  :passw, :email, :secret, :token, :_key, :crypt, :salt, :certificate, :otp, :ssn, :cvv, :cvc,
  # OAuth authorization codes and PKCE verifiers (MCP token exchange, consent
  # callbacks). Anchored so unrelated "*_code" parameters stay readable.
  /\Acode\z/i, :code_verifier
]
# The same filters applied to percent-decoded and nested names, which the
# "Started" and "Redirected to" lines otherwise see undecoded (ConsoleLogFilter).
Rails.application.config.filter_parameters += base_filters + [ ConsoleLogFilter.decoded_name_filter(base_filters) ]
