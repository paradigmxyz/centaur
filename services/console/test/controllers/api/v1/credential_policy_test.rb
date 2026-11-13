require "test_helper"

module Api
  module V1
    # The namespace decides the credential: every /api/v1/console/* action takes
    # the console session only, every other /api/v1 action (outside
    # /api/v1/sandbox) a bearer API key only. Nothing accepts both.
    class CredentialPolicyTest < ActionDispatch::IntegrationTest
      CONSOLE_PREFIX = "/api/v1/console/".freeze

      def self.operator_routes
        Rails.application.routes.routes.filter_map do |route|
          path = route.path.spec.to_s.sub("(.:format)", "")
          next unless path.start_with?("/api/v1/")
          next if path.start_with?("/api/v1/sandbox/")

          klass = "#{route.defaults[:controller]}_controller".camelize.safe_constantize
          [ path, route, klass ]
        end
      end

      def self.routed_requests
        operator_routes.flat_map do |path, route, klass|
          concrete = path.gsub(/:\w+/, "x1")
          route.verb.split("|").map { |verb| [ verb.downcase, concrete, klass, route.defaults[:action].to_s ] }
        end.uniq
      end

      def before_callbacks(klass)
        klass._process_action_callbacks.select { |cb| cb.kind == :before }.map(&:filter)
      end

      test "every /api/v1/console action is session-only and every other /api/v1 action is bearer-only" do
        routes = self.class.operator_routes
        console, bearer = routes.partition { |path, _route, _klass| path.start_with?(CONSOLE_PREFIX) }
        assert_operator console.size, :>, 80
        assert_operator bearer.size, :>, 80

        console.each do |path, route, klass|
          label = "#{route.verb} #{path} -> #{klass}"
          assert klass && klass < Api::BaseController, label
          assert klass.include?(ConsoleApiAuthentication), label
          assert_equal :session, klass.accepted_credential, label
          assert klass.controller_path.start_with?("api/v1/console/"), label
          callbacks = before_callbacks(klass)
          assert_includes callbacks, :authenticate_console_session!, label
          assert_not_includes callbacks, :authenticate_api_key!, label
        end

        bearer.each do |path, route, klass|
          label = "#{route.verb} #{path} -> #{klass}"
          assert klass && klass < Api::BaseController, label
          assert_not klass.include?(ConsoleApiAuthentication), label
          assert_not klass.include?(SandboxAdminAuthentication), label
          assert_equal :bearer, klass.accepted_credential, label
          assert_not klass.controller_path.start_with?("api/v1/console/"), label
          callbacks = before_callbacks(klass)
          assert_includes callbacks, :authenticate_api_key!, label
          assert_not_includes callbacks, :authenticate_console_session!, label
        end
      end

      test "every console controller is routed only under /api/v1/console" do
        Rails.application.eager_load!
        console_classes = Api::BaseController.descendants.select { |klass| klass.include?(ConsoleApiAuthentication) }
        assert_operator console_classes.size, :>, 20
        Rails.application.routes.routes.each do |route|
          klass = "#{route.defaults[:controller]}_controller".camelize.safe_constantize
          next unless console_classes.include?(klass)

          assert route.path.spec.to_s.start_with?(CONSOLE_PREFIX), "#{klass} is routed at #{route.path.spec}"
        end
      end

      test "sandbox admin controllers stay on sandbox token authentication" do
        sandbox = Rails.application.routes.routes.filter_map do |route|
          klass = "#{route.defaults[:controller]}_controller".camelize.safe_constantize
          klass if klass && klass < Api::BaseController && klass.include?(SandboxAdminAuthentication)
        end.uniq
        assert_operator sandbox.size, :>=, 9
        sandbox.each do |klass|
          callbacks = before_callbacks(klass)
          assert_includes callbacks, :authenticate_sandbox_admin!, klass.name
          assert_not_includes callbacks, :authenticate_api_key!, klass.name
          assert_not_includes callbacks, :authenticate_console_session!, klass.name
        end
      end

      test "every routed request refuses the other namespace's credential and anonymous callers" do
        requests = self.class.routed_requests
        assert_operator requests.size, :>, 160
        ENV["CENTAUR_CONSOLE_API_CLIENTS_ENABLED"] = "true"
        counts = -> { [ ApiKey.unscoped.count, ::Proxy.count, Principal.count, Role.count, Grant.count, ScheduledTask.count ] }
        before = counts.call

        requests.each do |verb, path, _klass, _action|
          label = "#{verb.upcase} #{path}"
          console = path.start_with?(CONSOLE_PREFIX)

          public_send(verb, path, as: :json)
          assert_equal 401, response.status, "#{label} anonymous: #{response.body}"
          assert_equal "session_required", json_body.dig("error", "details", "reason"), label if console

          if console
            [ bearer_headers, bearer_headers(ConsoleApiAuthHelper::MEMBER_TOKEN) ].each do |headers|
              public_send(verb, path, headers: headers, as: :json)
              assert_equal 403, response.status, "#{label} bearer: #{response.body}"
              assert_equal "session_required", json_body.dig("error", "details", "reason"), label
            end
          else
            public_send(verb, path, headers: bearer_headers(ConsoleApiAuthHelper::MEMBER_TOKEN), as: :json)
            assert_equal 403, response.status, "#{label} member bearer: #{response.body}"
            assert_equal "API key owner is not an admin", json_body.dig("error", "message"), label
          end
        end

        sign_in_console(users(:acme_admin))
        headers = csrf_headers
        requests.reject { |_verb, path, _klass, _action| path.start_with?(CONSOLE_PREFIX) }.each do |verb, path, _klass, _action|
          public_send(verb, path, headers: headers, as: :json)
          assert_equal 401, response.status, "#{verb.upcase} #{path} admin session: #{response.body}"
          assert_equal "invalid or missing API key", json_body.dig("error", "message"), "#{verb.upcase} #{path}"
        end

        assert_equal before, counts.call
      ensure
        ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")
      end

      test "a console-namespace controller without the console credential fails closed" do
        klass = Class.new(Api::BaseController) do
          def index = render(json: { data: "reached" })
        end
        stub_console_const(:UnguardedController, klass) do
          with_routing do |set|
            set.draw { get "api/v1/console/unguarded", to: "api/v1/console/unguarded#index" }
            assert_raises(Api::BaseController::CredentialNamespaceMismatch) do
              get "/api/v1/console/unguarded", headers: bearer_headers
            end

            Rails.stub(:env, ActiveSupport::EnvironmentInquirer.new("production")) do
              get "/api/v1/console/unguarded", headers: bearer_headers
            end
            assert_response :unauthorized
            assert_equal "credential_namespace_mismatch", json_body.dig("error", "details", "reason")
            assert_no_match(/reached/, response.body)
          end
        end
      end

      test "a bearer controller routed under /api/v1/console fails closed" do
        with_routing do |set|
          set.draw { get "api/v1/console/keys", to: "api/v1/api_keys#index" }
          assert_raises(Api::BaseController::CredentialNamespaceMismatch) do
            get "/api/v1/console/keys", headers: bearer_headers
          end
        end
      end

      test "a console controller routed outside /api/v1/console fails closed" do
        sign_in_console(users(:acme_admin))
        with_routing do |set|
          set.draw { get "api/v1/whoami", to: "api/v1/console/me#show" }
          assert_raises(Api::BaseController::CredentialNamespaceMismatch) { get "/api/v1/whoami" }
        end
      end

      test "allow_active_users exists only in the console API and never admits a bearer key" do
        assert_not Api::BaseController.respond_to?(:allow_active_users)
        assert_not Api::BaseController.respond_to?(:allow_pending_users)

        klass = Class.new(Api::V1::Console::BaseController) do
          allow_active_users
          def create = render(json: { data: current_user.email })
        end
        stub_console_const(:OpenToActiveUsersController, klass) do
          with_routing do |set|
            set.draw { post "api/v1/console/open", to: "api/v1/console/open_to_active_users#create" }
            post "/api/v1/console/open", headers: bearer_headers(ConsoleApiAuthHelper::MEMBER_TOKEN)
            assert_response :forbidden
            assert_equal "session_required", json_body.dig("error", "details", "reason")

            post "/api/v1/console/open", headers: bearer_headers
            assert_response :forbidden
            assert_equal "session_required", json_body.dig("error", "details", "reason")
          end
        end
      end

      test "a session cannot mint, list, or revoke API keys" do
        sign_in_console(users(:acme_admin))
        key = api_keys(:acme_ci_key)

        assert_no_difference -> { ApiKey.unscoped.count } do
          post api_v1_api_keys_path, params: { data: { name: "browser" } }, headers: csrf_headers, as: :json
        end
        assert_cookie_not_accepted
        assert_no_match(/iak_/, response.body)

        delete api_v1_api_key_path(key.oid), headers: csrf_headers
        assert_cookie_not_accepted
        assert_nil ApiKey.unscoped.find(key.id).deleted_at

        get api_v1_api_keys_path
        assert_cookie_not_accepted
      end

      test "a session cannot create, change, or delete proxies" do
        sign_in_console(users(:acme_admin))
        proxy = ::Proxy.create!(name: "policy-proxy")

        assert_no_difference -> { ::Proxy.count } do
          post api_v1_proxies_path, params: { data: { name: "browser" } }, headers: csrf_headers, as: :json
        end
        assert_cookie_not_accepted

        patch api_v1_proxy_path(proxy.oid), params: { data: { name: "renamed" } }, headers: csrf_headers, as: :json
        assert_cookie_not_accepted
        assert_equal "policy-proxy", proxy.reload.name

        delete api_v1_proxy_path(proxy.oid), headers: csrf_headers
        assert_cookie_not_accepted
        assert ::Proxy.exists?(proxy.id)
      end

      test "a session cannot write the bearer API resources the console also edits" do
        sign_in_console(users(:acme_admin))
        principal = principals(:acme_user_bob)

        assert_no_difference -> { Principal.count } do
          post api_v1_principals_path, params: { data: { foreign_id: "from-browser" } }, headers: csrf_headers, as: :json
        end
        assert_cookie_not_accepted

        patch api_v1_principal_path(principal.oid), params: { data: { name: "renamed" } }, headers: csrf_headers, as: :json
        assert_cookie_not_accepted
        assert_not_equal "renamed", principal.reload.name

        get api_v1_static_secrets_path
        assert_cookie_not_accepted
      end

      test "bearer keys still mint API keys and proxies" do
        post api_v1_api_keys_path, params: { data: { name: "ci" } }, headers: bearer_headers, as: :json
        assert_response :created
        post api_v1_proxies_path, params: { data: { name: "ci" } }, headers: bearer_headers, as: :json
        assert_response :created
      end

      test "a bearer key cannot revoke itself" do
        delete api_v1_api_key_path(api_keys(:acme_ci_key).oid), headers: bearer_headers
        assert_response :unprocessable_entity
        assert_nil ApiKey.unscoped.find(api_keys(:acme_ci_key).id).deleted_at
      end

      test "a bearer admin key cannot manage users" do
        member = users(:member_user)
        pending = users(:pending_user)

        post promote_api_v1_console_user_path(member.oid), headers: bearer_headers
        assert_session_required
        assert_not member.reload.admin?

        post approve_api_v1_console_user_path(pending.oid), headers: bearer_headers
        assert_session_required
        assert pending.reload.pending?

        post disable_api_v1_console_user_path(member.oid), headers: bearer_headers
        assert_session_required
        assert member.reload.active?
      end

      test "a bearer admin key cannot touch API clients" do
        ENV["CENTAUR_CONSOLE_API_CLIENTS_ENABLED"] = "true"
        client = ApiClient.create!(name: "policy-client", scopes: ApiClient::SCOPES, created_by: users(:acme_admin))
        digest = client.secret_digest

        post rotate_secret_api_v1_console_api_client_path(client.oid), headers: bearer_headers
        assert_session_required
        assert_equal digest, client.reload.secret_digest

        post attach_principal_api_v1_console_api_client_path(client.oid),
             params: { data: { principal: principals(:acme_user_bob).oid } }, headers: bearer_headers, as: :json
        assert_session_required
        assert_empty client.reload.api_client_principals
      ensure
        ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")
      end

      test "a bearer admin key cannot force-start a workflow" do
        original = Console::WorkflowsController.client_factory
        Console::WorkflowsController.client_factory = -> { flunk "the workflow API must not be reached" }
        post runs_api_v1_console_workflow_path("slack_sync"), headers: bearer_headers, as: :json
        assert_session_required
      ensure
        Console::WorkflowsController.client_factory = original
      end

      test "a bearer admin key cannot change system settings, delete principals, or register presets" do
        before = system_settings(:default).reload.attributes
        patch api_v1_console_system_settings_path, params: { data: { default_sandbox_repo_cache: "changed" } },
              headers: bearer_headers, as: :json
        assert_session_required
        assert_equal before, system_settings(:default).reload.attributes

        principal = principals(:acme_user_bob)
        delete api_v1_console_principal_path(principal.oid), headers: bearer_headers
        assert_session_required
        assert Principal.exists?(principal.id)

        assert_no_difference -> { OauthApp.count } do
          post preset_api_v1_console_oauth_apps_path(provider: "mercator"), headers: bearer_headers
        end
        assert_session_required
      end

      test "an Authorization header is never ignored in favour of the session cookie" do
        sign_in_console(users(:acme_admin))
        member = users(:member_user)

        post promote_api_v1_console_user_path(member.oid), headers: csrf_headers.merge(bearer_headers)
        assert_session_required
        assert_not member.reload.admin?

        post promote_api_v1_console_user_path(member.oid), headers: csrf_headers
        assert_response :ok
        assert member.reload.admin?
      end

      test "an admin bearer key with a session cookie still uses the bearer credential on the bearer API" do
        sign_in_console(users(:member_user))
        get api_v1_principals_path, headers: bearer_headers
        assert_response :ok
      end

      private

      def assert_cookie_not_accepted
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      def assert_session_required
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")
      end

      def stub_console_const(name, value)
        Api::V1::Console.const_set(name, value)
        yield
      ensure
        Api::V1::Console.send(:remove_const, name) if Api::V1::Console.const_defined?(name, false)
      end
    end
  end
end
