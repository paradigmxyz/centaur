require "test_helper"

class RequestRuleTest < ActiveSupport::TestCase
  def new_rule(attrs = {})
    RequestRule.new(attrs)
  end

  def create_rule!(attrs = {})
    RequestRule.create!(attrs)
  end

  test "is valid with host only" do
    r = new_rule(host: "api.example.com")
    assert r.valid?
  end

  test "is valid with cidr only" do
    r = new_rule(cidr: "10.0.0.0/8")
    assert r.valid?
  end

  test "is invalid with both host and cidr" do
    r = new_rule(host: "api.example.com", cidr: "10.0.0.0/8")
    assert_not r.valid?
    assert_includes r.errors[:base], "host and cidr are mutually exclusive"
  end

  test "is invalid with neither host nor cidr" do
    r = new_rule
    assert_not r.valid?
    assert_includes r.errors[:base], "either host or cidr must be present"
  end

  test "rejects malformed cidr" do
    r = new_rule(cidr: "not-a-cidr")
    assert_not r.valid?
    assert_includes r.errors[:cidr], "is not a valid CIDR"
  end

  test "accepts valid HTTP methods" do
    r = new_rule(host: "x", http_methods: %w[GET POST])
    assert r.valid?
  end

  test "accepts wildcard method" do
    r = new_rule(host: "x", http_methods: %w[*])
    assert r.valid?
  end

  test "rejects unknown HTTP method" do
    r = new_rule(host: "x", http_methods: %w[BOGUS])
    assert_not r.valid?
    assert r.errors[:http_methods].any? { |m| m.include?("BOGUS") }
  end

  test "rejects lowercase HTTP method" do
    r = new_rule(host: "x", http_methods: %w[get])
    assert_not r.valid?
    assert r.errors[:http_methods].any? { |m| m.include?("get") }
  end

  test "rejects TRACE (not in allowlist)" do
    r = new_rule(host: "x", http_methods: %w[TRACE])
    assert_not r.valid?
  end

  test "http_methods must be an array" do
    r = new_rule(host: "x", http_methods: "GET")
    assert_not r.valid?
    assert_includes r.errors[:http_methods], "must be an array"
  end

  test "accepts path with leading slash" do
    r = new_rule(host: "x", paths: [ "/v1/*" ])
    assert r.valid?
  end

  test "rejects path without leading slash" do
    r = new_rule(host: "x", paths: [ "v1/*" ])
    assert_not r.valid?
    assert r.errors[:paths].any? { |m| m.include?("v1/*") }
  end

  test "paths must be an array" do
    r = new_rule(host: "x", paths: "/v1")
    assert_not r.valid?
    assert_includes r.errors[:paths], "must be an array"
  end

  test "position defaults to 0 when not set" do
    RequestRule.delete_all
    a = create_rule!(host: "a.example.com")
    assert_equal 0, a.position
  end

  test "explicit position is respected" do
    RequestRule.delete_all
    a = create_rule!(host: "a.example.com", position: 5)
    assert_equal 5, a.position
  end

  test "default_scope orders by position" do
    RequestRule.delete_all
    c = create_rule!(host: "c.example.com", position: 2)
    a = create_rule!(host: "a.example.com", position: 0)
    b = create_rule!(host: "b.example.com", position: 1)
    assert_equal [ a, b, c ], RequestRule.all.to_a
  end

  test "declares rqr as its oid prefix" do
    assert_equal "rqr", RequestRule.oid_prefix
  end

  test "find_by_oid round-trips" do
    r = request_rules(:api_host)
    assert_equal r, RequestRule.find_by_oid(r.oid)
  end

  test "rejects belonging to more than one owner" do
    r = new_rule(host: "x",
                 static_secret: static_secrets(:github_token_inject),
                 oauth_token_secret: oauth_token_secrets(:acme_gmail_oauth))
    assert_not r.valid?
    assert_includes r.errors[:base], "must belong to at most one of static_secret, gcp_auth_secret, aws_auth_secret, oauth_token_secret, hmac_secret"
  end
end
