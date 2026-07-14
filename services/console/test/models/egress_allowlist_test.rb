require "test_helper"

# The egress allowlist: which destinations a sandbox may reach at all.
#
# The load-bearing distinction throughout is *omitted* vs *empty*. Omitting
# `rules` leaves iron-proxy with no allowlist transform, so nothing is blocked.
# Sending `rules: []` gives it an allowlist that matches nothing, so *everything*
# is blocked. The two are one keystroke apart and mean opposite things.
class EgressAllowlistTest < ActiveSupport::TestCase
  setup do
    @principal = principals(:acme_channel)
    @setting = SystemSetting.current
  end

  # -- off (the default, and every deployment before this) --------------------

  test "off omits rules entirely, so the proxy builds no allowlist transform" do
    assert_equal "off", @setting.egress_allowlist_mode

    config = @principal.effective_config(redact_secrets: false)

    # Not `assert_empty config["rules"]` -- the key must be ABSENT. An empty
    # array here would 403 every request the sandbox makes.
    refute config.key?("rules"), "off must omit `rules`, not send an empty list"
    refute config.key?("allowlist_warn")
  end

  # -- enforce ----------------------------------------------------------------

  test "enforce serves the base list unioned with every granted credential's hosts" do
    secret = static_secrets(:github_token_inject)
    secret.rules.create!(host: "api.github.com", position: 0)

    enable!(mode: "enforce", base: [ { "host" => "api.anthropic.com" } ])

    rules = @principal.effective_config(redact_secrets: false).fetch("rules")

    assert_includes rules, { "host" => "api.anthropic.com" }, "base list must survive"
    assert_includes rules, { "host" => "api.github.com" }, "granted credential's host must be derived"
  end

  test "a host reachable only via a granted credential needs no second list" do
    # The point of deriving: granting a tool its secret is what allows its host.
    # There is no separate allowlist to forget to update -- the failure mode
    # where a new tool 403s in production with CI green.
    secret = static_secrets(:github_token_inject)
    secret.rules.create!(host: "api.newtool.example", position: 0)

    enable!(mode: "enforce", base: [ { "host" => "api.anthropic.com" } ])

    rules = @principal.effective_config(redact_secrets: false).fetch("rules")
    assert_includes rules, { "host" => "api.newtool.example" }
  end

  test "a credential whose source cannot be delivered still allows its host" do
    # The allowlist derives from GRANTED credentials, not from the served subset
    # (which drops anything currently undeliverable). Otherwise a vault blip
    # silently rewrites the egress boundary and 403s a host that is still
    # perfectly well authorized. Losing a credential should cost the request its
    # credential -- never its destination.
    secret = static_secrets(:github_token_inject)
    secret.rules.create!(host: "api.github.com", position: 0)
    refute_includes @principal.send(:served_credentials)[:static], secret,
      "fixture guard: this secret must be undeliverable for the test to mean anything"

    enable!(mode: "enforce", base: [ { "host" => "api.anthropic.com" } ])

    rules = @principal.effective_config(redact_secrets: false).fetch("rules")
    assert_includes rules, { "host" => "api.github.com" }
  end

  test "a host in both the base list and a credential's rules appears once" do
    secret = static_secrets(:github_token_inject)
    secret.rules.create!(host: "api.github.com", position: 0)

    enable!(mode: "enforce", base: [ { "host" => "api.github.com" } ])

    rules = @principal.effective_config(redact_secrets: false).fetch("rules")
    assert_equal 1, rules.count { |rule| rule == { "host" => "api.github.com" } }
  end

  test "method and path scoping survives into the proxy rule" do
    secret = static_secrets(:github_token_inject)
    secret.rules.create!(host: "api.github.com", http_methods: [ "GET" ], paths: [ "/repos" ], position: 0)

    enable!(mode: "enforce", base: [ { "host" => "api.anthropic.com" } ])

    rules = @principal.effective_config(redact_secrets: false).fetch("rules")
    # `http_methods` on our side, `methods` on the proxy's -- RequestRule#to_proxy_rule
    # does the translation, and the allowlist is worthless if it silently drops it.
    assert_includes rules, {
      "host" => "api.github.com", "methods" => [ "GET" ], "paths" => [ "/repos" ]
    }
  end

  test "the sandbox's own control plane is allowed, or the allowlist kills every turn" do
    # Since #1002 sandbox->api traffic rides the proxy. If the API host is not in the
    # allowlist the sandbox cannot reach api-rs at all -- the allowlist takes the bot
    # DOWN rather than bounding it. The api-server JWT is a GENERATED secret, not a
    # granted one, so deriving only from grants misses it. That is the whole bug.
    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_API_URL" => "http://api.internal:8080",
      "CENTAUR_API_SERVER_PROXY_HOSTS" => nil
    ) do
      SlackChannelPermission.create!(
        principal: @principal, channel_id: "C0123456789", channel_name: "general",
        upload_enabled: true, download_enabled: false, history_enabled: true
      )
      enable!(mode: "enforce", base: [ { "host" => "api.anthropic.com" } ])

      rules = @principal.effective_config(redact_secrets: false).fetch("rules")
      assert_includes rules, { "host" => "api.internal" },
        "the control-plane host must survive into the allowlist"
    end
  end

  # -- warn (the audit pass before enforcing) ---------------------------------

  test "warn serves the same rules but tells the proxy not to enforce them" do
    enable!(mode: "warn", base: [ { "host" => "api.anthropic.com" } ])

    config = @principal.effective_config(redact_secrets: false)

    assert_includes config.fetch("rules"), { "host" => "api.anthropic.com" }
    assert_equal true, config.fetch("allowlist_warn")
  end

  test "enforce does not set the warn flag" do
    enable!(mode: "enforce", base: [ { "host" => "api.anthropic.com" } ])

    refute @principal.effective_config(redact_secrets: false).key?("allowlist_warn")
  end

  # -- the blackout guard -----------------------------------------------------

  test "the allowlist cannot be turned on with an empty base list" do
    # Without this guard: a principal holding no credentials derives no rules,
    # is served `rules: []`, and 403s every request it makes -- including the one
    # to the model provider. The sandbox is bricked on the proxy's next 5s poll.
    @setting.egress_allowlist_mode = "enforce"
    @setting.egress_allowlist_base_rules = []

    refute @setting.valid?
    assert_match(/denies everything/, @setting.errors[:egress_allowlist_base_rules].join)
  end

  test "a base rule must name a host or a cidr, and never both" do
    @setting.egress_allowlist_mode = "enforce"

    @setting.egress_allowlist_base_rules = [ { "http_methods" => [ "GET" ] } ]
    refute @setting.valid?, "a rule naming no destination is not a rule"

    @setting.egress_allowlist_base_rules = [ { "host" => "a.example", "cidr" => "10.0.0.0/8" } ]
    refute @setting.valid?, "host and cidr are mutually exclusive"

    @setting.egress_allowlist_base_rules = [ { "host" => "a.example" } ]
    assert @setting.valid?
  end

  test "an unknown mode is refused" do
    @setting.egress_allowlist_mode = "enforcing"
    refute @setting.valid?
  end

  private

  def enable!(mode:, base:)
    @setting.update!(egress_allowlist_mode: mode, egress_allowlist_base_rules: base)
  end

  def with_env(values)
    previous = values.keys.to_h { |key| [ key, ENV[key] ] }
    values.each do |key, value|
      value.nil? ? ENV.delete(key) : ENV[key] = value
    end
    yield
  ensure
    previous.each do |key, value|
      value.nil? ? ENV.delete(key) : ENV[key] = value
    end
  end
end
