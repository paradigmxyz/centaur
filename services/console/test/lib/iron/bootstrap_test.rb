require "test_helper"

class Iron::BootstrapTest < ActiveSupport::TestCase
  self.use_transactional_tests = true

  VALID_TOKEN = "iak_#{"a" * 64}".freeze

  setup do
    # bootstrap guard checks `User.exists?`; clear fixture-loaded users (and their dependents) so we exercise the empty-DB code path
    Grant.delete_all
    PrincipalRole.delete_all
    Proxy.delete_all
    RequestRule.delete_all
    SecretSource.delete_all
    BrokerCredential.delete_all
    OauthApp.delete_all
    StaticSecret.delete_all
    GcpAuthSecret.delete_all
    AwsAuthSecret.delete_all
    OauthTokenSecret.delete_all
    PgDsnSecret.delete_all
    HmacSecret.delete_all
    Principal.delete_all
    Role.delete_all
    ApiKey.unscoped.delete_all
    UserIdentity.delete_all
    User.delete_all
    @env = ENV.to_hash.slice(
      "CENTAUR_CONSOLE_INITIAL_USER_EMAIL",
      "CENTAUR_CONSOLE_INITIAL_USER_PASSWORD",
      "CENTAUR_CONSOLE_INITIAL_API_KEY"
    )
  end

  teardown do
    %w[CENTAUR_CONSOLE_INITIAL_USER_EMAIL CENTAUR_CONSOLE_INITIAL_USER_PASSWORD CENTAUR_CONSOLE_INITIAL_API_KEY].each do |k|
      ENV.delete(k)
    end
    @env.each { |k, v| ENV[k] = v }
  end

  def set_env(email: nil, password: nil, api_key: nil)
    ENV["CENTAUR_CONSOLE_INITIAL_USER_EMAIL"] = email
    ENV["CENTAUR_CONSOLE_INITIAL_USER_PASSWORD"] = password
    ENV["CENTAUR_CONSOLE_INITIAL_API_KEY"] = api_key
  end

  test "no-op when email not set" do
    set_env(email: nil)
    assert_no_difference -> { User.count } do
      Iron::Bootstrap.run!
    end
  end

  test "no-op when a user already exists" do
    User.create!(email: "preexisting@example.com", password: "password123456")
    set_env(email: "boot@example.com", password: "password123456", api_key: VALID_TOKEN)
    assert_no_difference -> { User.count } do
      assert_no_difference -> { ApiKey.unscoped.count } do
        Iron::Bootstrap.run!
      end
    end
  end

  test "creates user and api key when env vars supplied and DB empty" do
    set_env(email: "boot@example.com", password: "password123456", api_key: VALID_TOKEN)
    Iron::Bootstrap.run!
    user = User.find_by!(email: "boot@example.com")
    assert_equal user, user.authenticate("password123456")
    assert_equal user, ApiKey.find_by_token(VALID_TOKEN).user
    assert user.active?, "the bootstrap operator must be active"
    assert user.admin?, "the bootstrap operator must be an admin"
  end

  test "honors a supplied API key token" do
    set_env(email: "boot@example.com", password: "password123456", api_key: VALID_TOKEN)
    Iron::Bootstrap.run!
    assert_not_nil ApiKey.find_by_token(VALID_TOKEN)
  end

  test "generates an API key when none supplied" do
    set_env(email: "boot@example.com", password: "password123456")
    Iron::Bootstrap.run!
    user = User.find_by!(email: "boot@example.com")
    assert_equal 1, user.api_keys.count
    assert_equal 64, user.api_keys.first.token_hash.length
  end

  test "raises when password missing" do
    set_env(email: "boot@example.com")
    assert_raises(Iron::Bootstrap::Error) { Iron::Bootstrap.run! }
  end

  test "raises when API key has wrong prefix" do
    set_env(email: "boot@example.com", password: "password123456", api_key: "wrong_#{"a" * 64}")
    assert_raises(Iron::Bootstrap::Error) { Iron::Bootstrap.run! }
  end

  test "raises when API key is too short" do
    set_env(email: "boot@example.com", password: "password123456", api_key: "iak_abc")
    assert_raises(Iron::Bootstrap::Error) { Iron::Bootstrap.run! }
  end

  test "raises when API key contains non-hex chars" do
    set_env(email: "boot@example.com", password: "password123456", api_key: "iak_#{"g" * 64}")
    assert_raises(Iron::Bootstrap::Error) { Iron::Bootstrap.run! }
  end

  test "raises when API key uses uppercase hex" do
    set_env(email: "boot@example.com", password: "password123456", api_key: "iak_#{"A" * 64}")
    assert_raises(Iron::Bootstrap::Error) { Iron::Bootstrap.run! }
  end
end
