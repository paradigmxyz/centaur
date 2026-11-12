require "test_helper"

module Api
  module V1
    class CredentialPolicyTest < ActionDispatch::IntegrationTest
      B = :bearer
      S = :session
      BS = :bearer_or_session

      SECRET_KINDS = %w[static gcp_auth gcp_id_token aws_auth oauth_token pg_dsn hmac ssh_key].freeze
      CRUD = %w[index show create update destroy lookup].freeze

      # The complete, intended table. A new route fails this test until its
      # policy is chosen here.
      EXPECTED = {
        "api_keys" => %w[index show create destroy].index_with(B),
        "proxies" => %w[index show create update destroy].index_with(B),
        **SECRET_KINDS.to_h { |kind| [ "#{kind}_secrets", CRUD.index_with(BS) ] },
        "broker_credentials" => CRUD.index_with(BS),
        "grants" => %w[show create destroy].index_with(BS),
        "grantee_grants" => { "index" => BS },
        "principal_roles" => %w[index create destroy].index_with(BS),
        "roles" => (CRUD + %w[upsert_slack_channel_permission]).index_with(BS)
          .merge(%w[replace_slack_channel_permissions destroy_slack_channel_permission].index_with(S)),
        "principals" => %w[index show create update lookup effective_config upsert_slack_channel_permission]
          .index_with(BS)
          .merge(%w[destroy effective_grants replace_slack_channel_permissions
                    destroy_slack_channel_permission].index_with(S)),
        "oauth_apps" => CRUD.index_with(BS).merge(%w[preset credentials provider_details].index_with(S)),
        # show is BS for the admin lookup; ?view=console is session-only (see below).
        "scheduled_tasks" => { "show" => BS }
          .merge(%w[index new create update destroy run slack_channel_options].index_with(S)),
        "users" => %w[index approve disable promote demote].index_with(S),
        "api_clients" => %w[index show create update destroy rotate_secret disable enable attach_principal
                            detach_principal make_default_principal].index_with(S),
        "system_settings" => %w[show update slack_external_teams replace_slack_external_teams].index_with(S),
        "workflows" => %w[index show runs create_run].index_with(S),
        "skills" => %w[index show create update destroy editor_candidates share unshare].index_with(S),
        "me" => { "show" => S },
        "personal_ssh_keys" => %w[show update destroy validate].index_with(S),
        "integrations" => { "index" => S },
        "oauth_providers" => { "index" => S },
        "secrets" => %w[index show].index_with(S),
        "secret_role_grants" => %w[index create destroy].index_with(S),
        "slack_channels" => { "index" => S },
        "sessions" => { "destroy" => S }
      }.freeze

      def self.routed_actions
        Rails.application.routes.routes.filter_map do |route|
          controller = route.defaults[:controller].to_s
          next unless controller.start_with?("api/v1/")

          klass = "#{controller}_controller".camelize.safe_constantize
          next unless klass && klass < Api::BaseController
          next if klass.include?(SandboxAdminAuthentication)

          [ klass, route.defaults[:action].to_s ]
        end.uniq
      end

      test "every /api/v1 action handled by Api::BaseController declares a credential policy" do
        actions = self.class.routed_actions
        assert_operator actions.size, :>, 100
        missing = actions.reject { |klass, action| klass.accepted_credentials_for(action) }
        assert_empty missing.map { |klass, action| "#{klass}##{action}" }
      end

      test "the declared policies match the intended table exactly" do
        actual = self.class.routed_actions.each_with_object({}) do |(klass, action), table|
          name = klass.controller_path.delete_prefix("api/v1/")
          (table[name] ||= {})[action] = klass.accepted_credentials_for(action)
        end
        assert_equal EXPECTED.keys.sort, actual.keys.sort
        EXPECTED.each do |controller, actions|
          assert_equal actions.sort.to_h, actual[controller].sort.to_h, controller
        end
      end

      test "the scheduled task console view is session-only" do
        assert_equal [ [ S, :console_view? ] ], ScheduledTasksController.conditional_accepted_credentials_for(:show)
      end

      test "sandbox admin controllers stay on sandbox token authentication" do
        sandbox = Rails.application.routes.routes.filter_map do |route|
          klass = "#{route.defaults[:controller]}_controller".camelize.safe_constantize
          klass if klass && klass < Api::BaseController && klass.include?(SandboxAdminAuthentication)
        end.uniq
        assert_operator sandbox.size, :>=, 9
        sandbox.each do |klass|
          names = klass._process_action_callbacks.select { |cb| cb.kind == :before }.map(&:filter)
          assert_includes names, :authenticate_sandbox_admin!, klass.name
          assert_not_includes names, :authenticate_request!, klass.name
        end
      end

      test "an action without a declaration fails closed" do
        klass = Class.new(Api::BaseController) do
          def index = render(json: { data: "reached" })
        end
        assert_nil klass.accepted_credentials_for(:index)
        stub_const(:UndeclaredCredentialsController, klass) do
          with_routing do |set|
            set.draw { get "undeclared", to: "api/v1/credential_policy_test/undeclared_credentials#index" }
            assert_raises(Api::BaseController::MissingCredentialPolicy) do
              get "/undeclared", headers: bearer_headers
            end

            Rails.stub(:env, ActiveSupport::EnvironmentInquirer.new("production")) do
              get "/undeclared", headers: bearer_headers
            end
            assert_response :unauthorized
            assert_equal "credential_policy_missing", json_body.dig("error", "details", "reason")
            assert_no_match(/reached/, response.body)
          end
        end
      end

      test "accepts_credentials rejects unknown policies" do
        assert_raises(ArgumentError) { Class.new(Api::BaseController) { accepts_credentials :cookie } }
      end

      test "a session cannot mint, list, or revoke API keys" do
        sign_in_console(users(:acme_admin))
        key = api_keys(:acme_ci_key)

        assert_no_difference -> { ApiKey.unscoped.count } do
          post api_v1_api_keys_path, params: { data: { name: "browser" } }, headers: csrf_headers, as: :json
        end
        assert_bearer_required
        assert_no_match(/iak_/, response.body)

        delete api_v1_api_key_path(key.oid), headers: csrf_headers
        assert_bearer_required
        assert_nil ApiKey.unscoped.find(key.id).deleted_at

        get api_v1_api_keys_path
        assert_bearer_required
      end

      test "a session cannot create, change, or delete proxies" do
        sign_in_console(users(:acme_admin))
        proxy = ::Proxy.create!(name: "policy-proxy")

        assert_no_difference -> { ::Proxy.count } do
          post api_v1_proxies_path, params: { data: { name: "browser" } }, headers: csrf_headers, as: :json
        end
        assert_bearer_required

        patch api_v1_proxy_path(proxy.oid), params: { data: { name: "renamed" } }, headers: csrf_headers, as: :json
        assert_bearer_required
        assert_equal "policy-proxy", proxy.reload.name

        delete api_v1_proxy_path(proxy.oid), headers: csrf_headers
        assert_bearer_required
        assert ::Proxy.exists?(proxy.id)
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

        post promote_api_v1_user_path(member.oid), headers: bearer_headers
        assert_session_required
        assert_not member.reload.admin?

        post approve_api_v1_user_path(pending.oid), headers: bearer_headers
        assert_session_required
        assert pending.reload.pending?

        post disable_api_v1_user_path(member.oid), headers: bearer_headers
        assert_session_required
        assert member.reload.active?
      end

      test "a bearer admin key cannot touch API clients" do
        ENV["CENTAUR_CONSOLE_API_CLIENTS_ENABLED"] = "true"
        client = ApiClient.create!(name: "policy-client", scopes: ApiClient::SCOPES, created_by: users(:acme_admin))
        digest = client.secret_digest

        post rotate_secret_api_v1_api_client_path(client.oid), headers: bearer_headers
        assert_session_required
        assert_equal digest, client.reload.secret_digest

        post attach_principal_api_v1_api_client_path(client.oid),
             params: { data: { principal: principals(:acme_user_bob).oid } }, headers: bearer_headers, as: :json
        assert_session_required
        assert_empty client.reload.api_client_principals
      ensure
        ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")
      end

      test "a bearer admin key cannot force-start a workflow" do
        original = WorkflowsController.client_factory
        WorkflowsController.client_factory = -> { flunk "the workflow API must not be reached" }
        post runs_api_v1_workflow_path("slack_sync"), headers: bearer_headers, as: :json
        assert_session_required
      ensure
        WorkflowsController.client_factory = original
      end

      test "a bearer admin key cannot change system settings, delete principals, or register presets" do
        before = system_settings(:default).reload.attributes
        patch api_v1_system_settings_path, params: { data: { default_sandbox_repo_cache: "changed" } },
              headers: bearer_headers, as: :json
        assert_session_required
        assert_equal before, system_settings(:default).reload.attributes

        principal = principals(:acme_user_bob)
        delete api_v1_principal_path(principal.oid), headers: bearer_headers
        assert_session_required
        assert Principal.exists?(principal.id)

        assert_no_difference -> { OauthApp.count } do
          post preset_api_v1_oauth_apps_path(provider: "mercator"), headers: bearer_headers
        end
        assert_session_required
      end

      test "an Authorization header is never ignored in favour of the session cookie" do
        sign_in_console(users(:acme_admin))
        member = users(:member_user)

        post promote_api_v1_user_path(member.oid), headers: csrf_headers.merge(bearer_headers)
        assert_session_required
        assert_not member.reload.admin?

        post promote_api_v1_user_path(member.oid), headers: csrf_headers
        assert_response :ok
        assert member.reload.admin?
      end

      def self.routed_requests
        Rails.application.routes.routes.filter_map do |route|
          controller = route.defaults[:controller].to_s
          next unless controller.start_with?("api/v1/")

          klass = "#{controller}_controller".camelize.safe_constantize
          next unless klass && klass < Api::BaseController
          next if klass.include?(SandboxAdminAuthentication)

          path = route.path.spec.to_s.sub("(.:format)", "").gsub(/:\w+/, "x1")
          route.verb.split("|").map { |verb| [ verb.downcase, path, klass, route.defaults[:action].to_s ] }
        end.flatten(1).uniq
      end

      test "a non-admin owner's key is refused on every endpoint, including allow_active_users ones" do
        requests = self.class.routed_requests
        assert_operator requests.size, :>, 100
        member = users(:member_user)
        assert member.active?
        assert_not member.admin?
        ENV["CENTAUR_CONSOLE_API_CLIENTS_ENABLED"] = "true"
        requests.each do |verb, path, klass, action|
          public_send(verb, path, headers: bearer_headers(ConsoleApiAuthHelper::MEMBER_TOKEN), as: :json)
          expected = klass.accepted_credentials_for(action) == :session ? "session_required" : "admin_required"
          assert_equal 403, response.status, "#{verb.upcase} #{path}: #{response.body}"
          assert_equal expected, json_body.dig("error", "details", "reason"), "#{verb.upcase} #{path}"
        end
        assert_equal 0, member.scheduled_tasks.count
      ensure
        ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")
      end

      test "allow_active_users never admits a non-admin owner's key" do
        klass = Class.new(Api::BaseController) do
          accepts_credentials :bearer_or_session
          allow_active_users
          def create = render(json: { data: current_user.email })
        end
        stub_const(:OpenToActiveUsersController, klass) do
          with_routing do |set|
            set.draw { post "open", to: "api/v1/credential_policy_test/open_to_active_users#create" }
            post "/open", headers: bearer_headers(ConsoleApiAuthHelper::MEMBER_TOKEN)
            assert_response :forbidden
            assert_equal "admin_required", json_body.dig("error", "details", "reason")

            post "/open", headers: bearer_headers
            assert_response :ok
            assert_equal users(:acme_admin).email, json_body["data"]
          end
        end
      end

      private

      def assert_bearer_required
        assert_response :forbidden
        assert_equal "bearer_required", json_body.dig("error", "details", "reason")
      end

      def assert_session_required
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")
      end

      def stub_const(name, value)
        self.class.const_set(name, value)
        yield
      ensure
        self.class.send(:remove_const, name) if self.class.const_defined?(name, false)
      end
    end
  end
end
