require "test_helper"

module Api
  module V1
    class PrincipalsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_principal_url(id: "prn_unknown")
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      test "rejects requests with an unknown bearer token" do
        get api_v1_principal_url(id: "prn_unknown"),
            headers: auth_headers("iak_not-a-real-token")
        assert_response :unauthorized
      end

      test "rejects requests with a malformed Authorization scheme" do
        get api_v1_principal_url(id: "prn_unknown"),
            headers: { "Authorization" => "Token #{ACME_TOKEN}" }
        assert_response :unauthorized
      end

      test "GET returns a Principal with its labels" do
        principal = principals(:acme_channel)

        get api_v1_principal_url(id: principal.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal principal.oid, data["id"]
        assert_equal "default", data["namespace"]
        assert_equal "C0123456789", data["foreign_id"]
        PrincipalIdentityLabels.columns.each { |field| assert_not data.key?(field) }
        assert_equal(
          {
            "kind" => "slack_channel",
            "team" => "platform",
            "slack_channel_id" => "C0123456789",
            Principal::SANDBOX_REPO_CACHE_LABEL => "all"
          },
          data["labels"]
        )
        assert_equal "all", data["sandbox_repo_cache"]
        assert_not data.key?("sandbox_repo_cache_enabled")
        assert_equal true, data["sandbox_observability_enabled"]
        assert_equal true, data["sandbox_api_server_enabled"]
      end

      test "GET returns 404 for an unknown oid" do
        get api_v1_principal_url(id: "prn_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET does not resolve a foreign_id passed as :id" do
        principal = principals(:acme_channel)

        get api_v1_principal_url(id: principal.foreign_id), headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a Principal" do
        body = {
          data: {
            foreign_id: "U-new-id",
            labels: { "kind" => "user", "team" => "platform" },
            slack_channel_permissions: [
              {
                channel_id: "C0123456789",
                upload_enabled: true,
                download_enabled: false,
                history_enabled: true
              }
            ]
          }
        }

        assert_difference -> { Principal.count } => 1 do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_match(/\Aprn_/, data["id"])
        assert_equal "default", data["namespace"]
        assert_equal "U-new-id", data["foreign_id"]
        assert_equal(
          {
            "kind" => "user",
            "team" => "platform",
            Principal::SANDBOX_REPO_CACHE_LABEL => "all"
          },
          data["labels"]
        )
        assert_equal(
          [
            {
              "channel_id" => "C0123456789",
              "upload_enabled" => true,
              "download_enabled" => false,
              "history_enabled" => true
            }
          ],
          data["slack_channel_permissions"]
        )
        assert_equal "all", data["sandbox_repo_cache"]
        assert_not data.key?("sandbox_repo_cache_enabled")
        assert_equal true, data["sandbox_observability_enabled"]
        assert_equal true, data["sandbox_api_server_enabled"]
      end

      test "POST applies system sandbox defaults when omitted" do
        system_settings(:default).update!(
          default_sandbox_repo_cache: "public",
          default_sandbox_observability_enabled: false,
          default_sandbox_api_server_enabled: false
        )
        body = {
          data: {
            foreign_id: "U-defaulted"
          }
        }

        post api_v1_principals_url, params: body.to_json, headers: auth_headers
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "public", data["sandbox_repo_cache"]
        assert_equal false, data["sandbox_observability_enabled"]
        assert_equal false, data["sandbox_api_server_enabled"]
      end

      test "POST applies all configured default roles" do
        Role.update_all(assign_by_default: false)
        roles(:acme_infra).update!(assign_by_default: true)
        roles(:globex_infra).update!(assign_by_default: true)
        body = { data: { foreign_id: "U-default-roles" } }

        post api_v1_principals_url, params: body.to_json, headers: auth_headers
        assert_response :created

        principal = Principal.find_by!(foreign_id: "U-default-roles")
        expected = [ roles(:acme_infra), roles(:globex_infra) ].sort_by(&:id)
        assert_equal expected, principal.roles.order(:id).to_a
      end

      test "POST keeps explicit sandbox capabilities over system defaults" do
        system_settings(:default).update!(
          default_sandbox_repo_cache: "none",
          default_sandbox_observability_enabled: false,
          default_sandbox_api_server_enabled: false
        )
        body = {
          data: {
            foreign_id: "U-explicit-capabilities",
            sandbox_repo_cache: "all",
            sandbox_observability_enabled: true,
            sandbox_api_server_enabled: true
          }
        }

        post api_v1_principals_url, params: body.to_json, headers: auth_headers
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "all", data["sandbox_repo_cache"]
        assert_equal true, data["sandbox_observability_enabled"]
        assert_equal true, data["sandbox_api_server_enabled"]
      end

      test "POST overwrites explicit repo-cache label with system default" do
        system_settings(:default).update!(default_sandbox_repo_cache: "all")
        body = {
          data: {
            foreign_id: "U-explicit-repo-cache-label",
            labels: { Principal::SANDBOX_REPO_CACHE_LABEL => "none" }
          }
        }

        post api_v1_principals_url, params: body.to_json, headers: auth_headers
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "all", data["sandbox_repo_cache"]
        assert_equal(
          { Principal::SANDBOX_REPO_CACHE_LABEL => "all", "kind" => "unknown" },
          data["labels"]
        )
      end

      test "POST uses repo-cache param over conflicting label" do
        system_settings(:default).update!(default_sandbox_repo_cache: "all")
        body = {
          data: {
            foreign_id: "U-repo-cache-param-wins",
            sandbox_repo_cache: "public",
            labels: { Principal::SANDBOX_REPO_CACHE_LABEL => "none" }
          }
        }

        post api_v1_principals_url, params: body.to_json, headers: auth_headers
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "public", data["sandbox_repo_cache"]
        assert_equal(
          { Principal::SANDBOX_REPO_CACHE_LABEL => "public", "kind" => "unknown" },
          data["labels"]
        )
      end

      test "POST creates a Principal with only a human-readable name" do
        body = { data: { name: "Just a label" } }

        assert_difference -> { Principal.count } => 1 do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "Just a label", data["name"]
        assert_equal "default", data["namespace"]
        assert_nil data["foreign_id"]
      end

      test "PUT updates the human-readable name" do
        principal = principals(:acme_channel)
        principal.update!(
          sandbox_repo_cache: "none",
          sandbox_observability_enabled: false,
          sandbox_api_server_enabled: false
        )
        body = { data: { name: "Acme Slack channel" } }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal "Acme Slack channel", principal.name
        assert_equal "none", principal.sandbox_repo_cache
        assert_equal false, principal.sandbox_observability_enabled
        assert_equal false, principal.sandbox_api_server_enabled
      end

      test "PUT updates sandbox access flags" do
        principal = principals(:acme_channel)
        body = {
          data: {
            sandbox_repo_cache: "public",
            sandbox_observability_enabled: false,
            sandbox_api_server_enabled: false
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal "public", principal.sandbox_repo_cache
        assert_equal false, principal.sandbox_observability_enabled
        assert_equal false, principal.sandbox_api_server_enabled

        data = json_body.fetch("data")
        assert_equal "public", data["sandbox_repo_cache"]
        assert_not data.key?("sandbox_repo_cache_enabled")
        assert_equal false, data["sandbox_observability_enabled"]
        assert_equal false, data["sandbox_api_server_enabled"]
      end

      test "POST returns 422 when foreign_id already exists" do
        existing = principals(:acme_channel)
        body = {
          data: { foreign_id: existing.foreign_id }
        }

        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "POST returns 400 when the data key is missing" do
        post api_v1_principals_url, params: {}.to_json, headers: auth_headers
        assert_response :bad_request
      end

      test "POST creates a Slack principal from first-class identity fields" do
        post api_v1_principals_url,
             params: {
               data: {
                 foreign_id: "direct-slack-identity",
                 kind: "slack_dm",
                 slack_user_id: "U0123456789",
                 slack_team_id: "T0123456789",
                 slack_email: "ada@example.com",
                 labels: { "managed-by" => "centaur" }
               }
             }.to_json,
             headers: auth_headers

        assert_response :created
        principal = Principal.find_by!(foreign_id: "direct-slack-identity")
        assert_equal "slack_dm", principal.kind
        assert_equal "U0123456789", principal.slack_user_id
        assert_equal "T0123456789", principal.slack_team_id
        assert_equal "ada@example.com", principal.slack_email
        assert_equal "centaur", principal.labels["managed-by"]
        PrincipalIdentityLabels.columns.each do |field|
          assert_not json_body.fetch("data").key?(field)
        end
      end

      test "POST accepts matching first-class fields and compatibility labels" do
        user = users(:acme_admin)

        post api_v1_principals_url,
             params: {
               data: {
                 foreign_id: "matching-console-user-identity",
                 kind: "console_user",
                 console_user_id: user.id,
                 console_user_email: user.email,
                 labels: {
                   "kind" => "console_user",
                   "console-user-id" => user.oid,
                   "email" => user.email,
                   "managed-by" => "centaur"
                 }
               }
             }.to_json,
             headers: auth_headers

        assert_response :created
        principal = Principal.find_by!(foreign_id: "matching-console-user-identity")
        assert_equal user.id, principal.console_user_id
        assert_equal user.email, principal.console_user_email
        assert_equal "centaur", principal.labels["managed-by"]
        assert_empty principal.labels.slice("kind", "console-user-id", "email")
      end

      test "POST returns 422 when first-class Slack fields disagree with compatibility labels" do
        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url,
               params: {
                 data: {
                   foreign_id: "conflicting-slack-identity",
                   kind: "slack_dm",
                   slack_user_id: "U0123456789",
                   slack_channel_id: "D0123456789",
                   slack_team_id: "T0123456789",
                   slack_email: "ada@example.com",
                   labels: {
                     "kind" => "slack_channel",
                     "slack_user_id" => "U9876543210",
                     "slack_channel_id" => "D9876543210",
                     "slack_team_id" => "T9876543210",
                     "slack_email" => "grace@example.com"
                   }
                 }
               }.to_json,
               headers: auth_headers
        end

        assert_response :unprocessable_content
        details = json_body.dig("error", "details")
        %w[kind slack_user_id slack_channel_id slack_team_id slack_email].each do |field|
          assert_includes details.fetch(field), "does not agree with labels.#{field}"
        end
      end

      test "POST returns 422 when first-class console user fields disagree with compatibility labels" do
        user = users(:acme_admin)
        other_user = users(:globex_admin)

        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url,
               params: {
                 data: {
                   foreign_id: "conflicting-console-user-identity",
                   kind: "console_user",
                   console_user_id: user.id,
                   console_user_email: user.email,
                   labels: {
                     "kind" => "console_user",
                     "console-user-id" => other_user.oid,
                     "email" => other_user.email
                   }
                 }
               }.to_json,
               headers: auth_headers
        end

        assert_response :unprocessable_content
        details = json_body.dig("error", "details")
        assert_includes details.fetch("console_user_id"), "does not agree with labels.console-user-id"
        assert_includes details.fetch("console_user_email"), "does not agree with labels.email"
      end

      test "PUT returns 422 when an unchanged first-class field disagrees with a label" do
        principal = principals(:acme_user_alice)

        put api_v1_principal_url(id: principal.oid),
            params: {
              data: {
                kind: principal.kind,
                slack_user_id: principal.slack_user_id,
                labels: {
                  "kind" => principal.kind,
                  "slack_user_id" => "U9876543210"
                }
              }
            }.to_json,
            headers: auth_headers

        assert_response :unprocessable_content
        assert_includes json_body.dig("error", "details", "slack_user_id"),
                        "does not agree with labels.slack_user_id"
      end

      test "PUT updates labels" do
        principal = principals(:acme_channel)
        body = {
          data: { labels: { "kind" => "slack_channel", "team" => "ops" } }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal(
          {
            "team" => "ops",
            Principal::SANDBOX_REPO_CACHE_LABEL => "all"
          },
          principal.labels
        )
        assert_equal "slack_channel", json_body.dig("data", "labels", "kind")
        assert_equal "C0123456789", json_body.dig("data", "labels", "slack_channel_id")
      end

      test "PUT promotes identity labels into columns and normalizes blank Slack values" do
        principal = principals(:acme_channel)
        body = {
          data: {
            labels: {
              "kind" => "slack_dm",
              "slack_user_id" => "U0123456789",
              "slack_channel_id" => "  ",
              "slack_team_id" => "T0123456789",
              "slack_email" => "ada@example.com",
              "team" => "ops"
            }
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal "slack_dm", principal.kind
        assert_equal "U0123456789", principal.slack_user_id
        assert_nil principal.slack_channel_id
        assert_equal "T0123456789", principal.slack_team_id
        assert_equal "ada@example.com", principal.slack_email
        assert_equal(
          { "team" => "ops", Principal::SANDBOX_REPO_CACHE_LABEL => "all" },
          principal.labels
        )
        assert_equal "slack_dm", json_body.dig("data", "labels", "kind")
        assert_equal "U0123456789", json_body.dig("data", "labels", "slack_user_id")
        assert_not json_body.dig("data", "labels").key?("slack_channel_id")
        assert_equal "T0123456789", json_body.dig("data", "labels", "slack_team_id")
        assert_equal "ada@example.com", json_body.dig("data", "labels", "slack_email")
      end

      test "PUT rejects a blank kind" do
        principal = principals(:acme_channel)

        put api_v1_principal_url(id: principal.oid),
            params: { data: { labels: { kind: "  " } } }.to_json,
            headers: auth_headers

        assert_response :unprocessable_content
        assert_includes json_body.dig("error", "details", "kind"), "can't be blank"
      end

      test "PUT rejects unknown kinds and malformed Slack identity fields" do
        principal = principals(:acme_channel)

        put api_v1_principal_url(id: principal.oid),
            params: {
              data: {
                labels: {
                  kind: "future_platform",
                  slack_user_id: " U0123456789 ",
                  slack_channel_id: "C123",
                  slack_team_id: "t0123456789",
                  slack_email: "not-an-email"
                }
              }
            }.to_json,
            headers: auth_headers

        assert_response :unprocessable_content
        details = json_body.dig("error", "details")
        %w[kind slack_user_id slack_channel_id slack_team_id slack_email].each do |field|
          assert_predicate details.fetch(field), :any?
        end
      end

      test "PUT overwrites explicit repo-cache label" do
        principal = principals(:acme_channel)
        body = {
          data: {
            labels: {
              "kind" => "slack_channel",
              Principal::SANDBOX_REPO_CACHE_LABEL => "none"
            }
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok
        assert_equal "all", principal.reload.sandbox_repo_cache
        assert_equal "all", principal.labels[Principal::SANDBOX_REPO_CACHE_LABEL]
      end

      test "PUT replaces Slack channel permission rows" do
        principal = principals(:acme_channel)
        SlackChannelPermission.create!(
          principal: principal,
          channel_id: "C1111111111",
          upload_enabled: true
        )
        body = {
          data: {
            slack_channel_permissions: [
              {
                channel_id: "C0123456789",
                upload_enabled: true,
                download_enabled: true,
                history_enabled: false
              },
              {
                channel_id: "G9876543210",
                upload_enabled: false,
                download_enabled: false,
                history_enabled: true
              }
            ]
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        assert_equal(
          [
            {
              "channel_id" => "C0123456789",
              "upload_enabled" => true,
              "download_enabled" => true,
              "history_enabled" => false
            },
            {
              "channel_id" => "G9876543210",
              "upload_enabled" => false,
              "download_enabled" => false,
              "history_enabled" => true
            }
          ],
          principal.reload.slack_channel_permissions_payload
        )
      end

      test "GET separates direct and effective Slack channel permissions" do
        principal = principals(:acme_channel)
        SlackChannelPermission.create!(
          principal: principal,
          channel_id: "C0123456789",
          upload_enabled: true
        )
        roles(:acme_infra).slack_channel_permissions.create!(
          channel_id: "C0123456789",
          download_enabled: true,
          history_enabled: true
        )

        get api_v1_principal_url(id: principal.oid), headers: auth_headers
        assert_response :ok
        assert_equal(
          {
            "channel_id" => "C0123456789",
            "upload_enabled" => true,
            "download_enabled" => false,
            "history_enabled" => false
          },
          json_body.dig("data", "slack_channel_permissions").sole
        )
        assert_equal(
          {
            "channel_id" => "C0123456789",
            "upload_enabled" => true,
            "download_enabled" => true,
            "history_enabled" => true
          },
          json_body.dig("data", "effective_slack_channel_permissions").sole
        )

        returned = json_body.fetch("data")
        put api_v1_principal_url(id: principal.oid),
            params: { data: returned }.to_json,
            headers: auth_headers
        assert_response :ok

        principal.principal_roles.find_by!(role: roles(:acme_infra)).destroy!
        assert_equal [ "C0123456789" ], principal.reload.slack_channel_permissions.pluck(:channel_id)
        direct = principal.slack_channel_permissions.sole
        assert_not direct.download_enabled
        assert_not direct.history_enabled
      end

      test "GET and PUT round-trip preserves malformed migrated identity values" do
        principal = principals(:acme_user_alice)
        principal.update_columns(
          slack_user_id: "U12345",
          slack_channel_id: "D123",
          slack_team_id: "TACME",
          slack_email: "pending"
        )

        get api_v1_principal_url(id: principal.oid), headers: auth_headers
        assert_response :ok
        returned = json_body.fetch("data")
        returned.fetch("labels")["team"] = "identity-platform"

        put api_v1_principal_url(id: principal.oid),
            params: { data: returned }.to_json,
            headers: auth_headers
        assert_response :ok

        principal.reload
        assert_equal "U12345", principal.slack_user_id
        assert_equal "D123", principal.slack_channel_id
        assert_equal "TACME", principal.slack_team_id
        assert_equal "pending", principal.slack_email
        assert_equal "identity-platform", principal.labels["team"]
        assert_empty principal.labels.slice(*PrincipalIdentityLabels.labels_for(principal.kind))
      end

      test "POST leaves omitted flags unchanged on an existing permission" do
        principal = principals(:acme_channel)
        permission = principal.slack_channel_permissions.create!(
          channel_id: "C0123456789",
          upload_enabled: false,
          download_enabled: true,
          history_enabled: false
        )

        post "/api/v1/principals/#{principal.oid}/slack_channel_permissions",
             params: { data: { channel_id: permission.channel_id, history_enabled: true } }.to_json,
             headers: auth_headers
        assert_response :ok

        permission.reload
        assert_not permission.upload_enabled
        assert_predicate permission, :download_enabled
        assert_predicate permission, :history_enabled
      end

      test "PUT rejects a single Slack channel permission object" do
        principal = principals(:acme_channel)
        body = {
          data: {
            slack_channel_permissions: {
              channel_id: "C0123456789",
              upload_enabled: true,
              download_enabled: false,
              history_enabled: true
            }
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :unprocessable_content
        assert_equal "slack_channel_permissions must be an array", json_body.dig("error", "message")
      end

      test "PUT rejects malformed Slack channel permission rows" do
        principal = principals(:acme_channel)
        body = { data: { slack_channel_permissions: [ "not-an-object" ] } }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :unprocessable_content
        assert_equal "slack_channel_permissions rows must be objects", json_body.dig("error", "message")
      end

      test "PUT can clear Slack channel permission rows" do
        principal = principals(:acme_channel)
        principal.update!(labels: { Principal::SLACK_CHANNEL_ID_LABEL => "C0123456789" })
        SlackChannelPermission.create!(
          principal: principal,
          channel_id: "C0123456789",
          upload_enabled: true,
          download_enabled: true,
          history_enabled: true
        )
        body = { data: { slack_channel_permissions: [] } }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        assert_empty principal.reload.slack_channel_permissions
        assert_equal [], json_body.dig("data", "slack_channel_permissions")
      end

      test "POST upserts one Slack channel permission without replacing other rows" do
        principal = principals(:acme_channel)
        SlackChannelPermission.create!(
          principal: principal,
          channel_id: "G9876543210",
          upload_enabled: true,
          download_enabled: false,
          history_enabled: false
        )
        body = {
          data: {
            channel_id: "C0123456789",
            upload_enabled: true,
            download_enabled: true,
            history_enabled: true
          }
        }

        post "/api/v1/principals/#{principal.oid}/slack_channel_permissions",
             params: body.to_json,
             headers: auth_headers
        assert_response :created

        assert_equal(
          [ "C0123456789", "G9876543210" ],
          principal.reload.slack_channel_permissions.ordered.pluck(:channel_id)
        )
        assert_not json_body.fetch("data").key?("channel_name")
      end

      test "POST updates an existing Slack channel permission with normalized channel id" do
        principal = principals(:acme_channel)
        SlackChannelPermission.create!(
          principal: principal,
          channel_id: "C0123456789",
          upload_enabled: true,
          download_enabled: false,
          history_enabled: false
        )
        body = {
          data: {
            channel_id: " c0123456789 ",
            upload_enabled: false,
            download_enabled: true,
            history_enabled: true
          }
        }

        assert_no_difference -> { principal.slack_channel_permissions.count } do
          post "/api/v1/principals/#{principal.oid}/slack_channel_permissions",
               params: body.to_json,
               headers: auth_headers
        end
        assert_response :ok

        permission = principal.reload.slack_channel_permissions.sole
        assert_equal "C0123456789", permission.channel_id
        assert_not permission.upload_enabled
        assert_predicate permission, :download_enabled
        assert_predicate permission, :history_enabled
      end

      test "POST retries after concurrent Slack channel permission create wins" do
        principal = principals(:acme_channel)
        body = {
          data: {
            channel_id: "C0123456789",
            upload_enabled: false,
            download_enabled: true,
            history_enabled: false
          }
        }
        calls = 0
        original = Api::V1::PrincipalsController.instance_method(:save_slack_channel_permission!)

        Api::V1::PrincipalsController.define_method(:save_slack_channel_permission!) do |target_principal, attrs|
          calls += 1
          if calls == 1
            target_principal.slack_channel_permissions.create!(
              channel_id: attrs[:channel_id],
              upload_enabled: true,
              download_enabled: false,
              history_enabled: true
            )
            raise ActiveRecord::RecordNotUnique, "duplicate key value violates unique constraint"
          end

          original.bind_call(self, target_principal, attrs)
        end
        Api::V1::PrincipalsController.send(:private, :save_slack_channel_permission!)

        assert_difference -> { principal.slack_channel_permissions.count } => 1 do
          post "/api/v1/principals/#{principal.oid}/slack_channel_permissions",
               params: body.to_json,
               headers: auth_headers
        end
        assert_response :ok

        permission = principal.reload.slack_channel_permissions.sole
        assert_equal "C0123456789", permission.channel_id
        assert_not permission.upload_enabled
        assert_predicate permission, :download_enabled
        assert_not permission.history_enabled
        assert_equal 1, calls
      ensure
        Api::V1::PrincipalsController.define_method(:save_slack_channel_permission!, original)
        Api::V1::PrincipalsController.send(:private, :save_slack_channel_permission!)
      end

      test "POST returns a validation error when the uniqueness retry is invalid" do
        principal = principals(:acme_channel)
        body = {
          data: {
            channel_id: "C0123456789",
            upload_enabled: false,
            download_enabled: false,
            history_enabled: false
          }
        }
        original = Api::V1::PrincipalsController.instance_method(:save_slack_channel_permission!)

        Api::V1::PrincipalsController.define_method(:save_slack_channel_permission!) do |target_principal, attrs|
          target_principal.slack_channel_permissions.create!(
            channel_id: attrs[:channel_id],
            upload_enabled: true
          )
          raise ActiveRecord::RecordNotUnique, "duplicate key value violates unique constraint"
        end
        Api::V1::PrincipalsController.send(:private, :save_slack_channel_permission!)

        assert_difference -> { principal.slack_channel_permissions.count } => 1 do
          post "/api/v1/principals/#{principal.oid}/slack_channel_permissions",
               params: body.to_json,
               headers: auth_headers
        end
        assert_response :unprocessable_content
        assert_equal "validation failed", json_body.dig("error", "message")
        assert_includes json_body.dig("error", "details", "base"), "Select at least one Slack permission"
      ensure
        Api::V1::PrincipalsController.define_method(:save_slack_channel_permission!, original)
        Api::V1::PrincipalsController.send(:private, :save_slack_channel_permission!)
      end

      test "POST upserts one Slack DM permission" do
        principal = principals(:acme_user_bob)
        body = {
          data: {
            channel_id: "D0123456789"
          }
        }

        post "/api/v1/principals/#{principal.oid}/slack_channel_permissions",
             params: body.to_json,
             headers: auth_headers
        assert_response :created

        permission = principal.reload.slack_channel_permissions.sole
        assert_equal "D0123456789", permission.channel_id
        assert_predicate permission, :upload_enabled
        assert_predicate permission, :download_enabled
        assert_predicate permission, :history_enabled
      end

      test "PUT rejects a non-default compatibility namespace and keeps foreign_id immutable" do
        principal = principals(:acme_channel)
        original_namespace = "default"
        original_foreign_id = principal.foreign_id

        body = {
          data: {
            namespace: "other",
            foreign_id: "different-foreign-id",
            labels: { "kind" => "slack_channel" }
          }
        }

        put api_v1_principal_url(id: principal.oid), params: body.to_json, headers: auth_headers
        assert_response :unprocessable_content
        assert_equal Api::BaseController::NAMESPACE_ERROR, json_body.dig("error", "message")

        principal.reload
        assert_equal original_namespace, "default"
        assert_equal original_foreign_id, principal.foreign_id
      end

      test "PUT returns 404 for an unknown oid" do
        put api_v1_principal_url(id: "prn_nope"),
            params: { data: { labels: {} } }.to_json,
            headers: auth_headers
        assert_response :not_found
      end

      test "PUT upserts a new principal by foreign_id" do
        system_settings(:default).update!(
          default_sandbox_repo_cache: "public",
          default_sandbox_observability_enabled: false,
          default_sandbox_api_server_enabled: false
        )
        body = { data: { name: "Upserted" } }
        assert_difference -> { Principal.count } => 1 do
          put api_v1_principal_url(id: "U-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "default", data["namespace"]
        assert_equal "U-upsert", data["foreign_id"]
        assert_equal "Upserted", data["name"]
        assert_equal "public", data["sandbox_repo_cache"]
        assert_equal false, data["sandbox_observability_enabled"]
        assert_equal false, data["sandbox_api_server_enabled"]
      end

      test "PUT by foreign_id updates an existing principal without creating" do
        principal = principals(:acme_channel)
        body = { data: { name: "Renamed channel" } }
        assert_no_difference -> { Principal.count } do
          put api_v1_principal_url(id: principal.foreign_id), params: body.to_json, headers: auth_headers
        end
        assert_response :ok
        assert_equal "Renamed channel", principal.reload.name
      end

      test "PUT by foreign_id does not apply defaults to an existing roleless principal" do
        principal = principals(:acme_user_bob)
        principal.principal_roles.destroy_all
        roles(:acme_infra).update!(assign_by_default: true)
        body = { data: { name: "Still roleless" } }

        put api_v1_principal_url(id: principal.foreign_id), params: body.to_json, headers: auth_headers

        assert_response :ok
        assert_empty principal.reload.roles
      end

      test "GET index rejects requests without an Authorization header" do
        get api_v1_principals_url, params: { namespace: "default" }
        assert_response :unauthorized
      end

      test "GET index does not require a namespace" do
        get api_v1_principals_url, headers: auth_headers
        assert_response :ok
      end

      test "GET index returns all principals" do
        get api_v1_principals_url, params: {}.to_json, headers: auth_headers
        assert_response :ok

        body = json_body
        ids = body.fetch("data").map { |p| p["id"] }
        expected = Principal.pluck(:id).map { |id| Principal.find(id).oid }
        assert_equal expected.sort, ids.sort
        assert body["data"].all? { |p| p["namespace"] == "default" }
        assert_equal expected.length, body.dig("meta", "total")
      end

      test "GET index filters by a single label" do
        get api_v1_principals_url,
            params: { labels: { kind: "user" } },
            headers: auth_headers
        assert_response :ok

        foreign_ids = json_body.fetch("data").map { |p| p["foreign_id"] }
        assert_equal %w[U-alice U-bob U-overlap U987654321].sort, foreign_ids.sort
      end

      test "GET index filters promoted identity labels through columns" do
        principals(:acme_channel).update!(slack_team_id: "T0123456789", slack_email: "channel@example.com")

        get api_v1_principals_url,
            params: {
              labels: {
                kind: "slack_channel",
                slack_channel_id: "C0123456789",
                slack_team_id: "T0123456789",
                slack_email: "channel@example.com"
              }
            },
            headers: auth_headers
        assert_response :ok

        assert_equal [ "C0123456789" ], json_body.fetch("data").map { |p| p["foreign_id"] }
      end

      test "GET index filters console user compatibility labels through columns" do
        user = users(:acme_admin)
        Principal.create!(
          foreign_id: "console-user-admin",
          kind: "console_user",
          console_user_id: user.id,
          console_user_email: user.email,
          created_by: user
        )

        get api_v1_principals_url,
            params: {
              labels: {
                kind: "console_user",
                "console-user-id" => user.oid,
                email: user.email
              }
            },
            headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal [ "console-user-admin" ], data.map { |p| p["foreign_id"] }
        assert_equal user.oid, data.first.dig("labels", "console-user-id")
        assert_equal user.email, data.first.dig("labels", "email")
      end

      test "GET index still filters ordinary email labels" do
        Principal.create!(
          foreign_id: "ordinary-email-principal",
          kind: "user",
          labels: { "email" => "ordinary@example.com" },
          created_by: users(:acme_admin)
        )

        get api_v1_principals_url,
            params: { labels: { email: "ordinary@example.com" } },
            headers: auth_headers
        assert_response :ok

        assert_equal [ "ordinary-email-principal" ], json_body.fetch("data").map { |p| p["foreign_id"] }
      end

      test "GET index filters by sandbox repo-cache label" do
        get api_v1_principals_url,
            params: { labels: { Principal::SANDBOX_REPO_CACHE_LABEL => "all" } },
            headers: auth_headers
        assert_response :ok

        foreign_ids = json_body.fetch("data").map { |p| p["foreign_id"] }
        assert_equal %w[C0123456789 U-alice U-bob U-overlap U987654321].sort, foreign_ids.sort
      end

      test "GET index ANDs multiple label filters" do
        get api_v1_principals_url,
            params: { labels: { kind: "user", team: "platform" } },
            headers: auth_headers
        assert_response :ok

        foreign_ids = json_body.fetch("data").map { |p| p["foreign_id"] }
        assert_equal %w[U-alice U-overlap].sort, foreign_ids.sort
      end

      test "GET index returns compatibility namespace values" do
        get api_v1_principals_url,
            params: { labels: { kind: "user", team: "platform" } },
            headers: auth_headers
        assert_response :ok

        assert json_body.fetch("data").all? { |p| p["namespace"] == "default" }
      end

      test "GET index returns an empty array when no labels match" do
        get api_v1_principals_url,
            params: { labels: { kind: "nonexistent" } },
            headers: auth_headers
        assert_response :ok
        assert_equal [], json_body.fetch("data")
        assert_equal 0, json_body.dig("meta", "total")
      end

      test "GET index honors limit and page" do
        get api_v1_principals_url,
            params: { limit: 1, page: 2 },
            headers: auth_headers
        assert_response :ok

        body = json_body
        total = Principal.count
        assert_equal 1, body.fetch("data").length
        assert_equal 1, body.dig("meta", "limit")
        assert_equal 2, body.dig("meta", "page")
        assert_equal total, body.dig("meta", "total")
        assert_equal total, body.dig("meta", "total_pages")
      end

      test "GET lookup finds a principal by foreign_id" do
        principal = principals(:acme_channel)

        get lookup_api_v1_principals_url(foreign_id: principal.foreign_id),
            headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal principal.oid, data["id"]
        assert_equal "default", data["namespace"]
        assert_equal principal.foreign_id, data["foreign_id"]
      end

      test "GET lookup returns 404 when no principal matches" do
        get lookup_api_v1_principals_url(foreign_id: "U-does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup rejects unauthenticated requests" do
        get lookup_api_v1_principals_url(foreign_id: "U-alice")
        assert_response :unauthorized
      end

      test "GET lookup scopes by namespace" do
        get "/api/v1/principals/lookup/other/U-alice", headers: auth_headers
        assert_response :not_found
      end

      test "POST rejects a non-URL-safe foreign_id" do
        body = { data: { foreign_id: "bad/value" } }
        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "POST rejects a non-default namespace" do
        body = { data: { namespace: "bad/value", foreign_id: "U-ok" } }
        assert_no_difference -> { Principal.count } do
          post api_v1_principals_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "GET index clamps limit above the max" do
        get api_v1_principals_url,
            params: { limit: 9999 },
            headers: auth_headers
        assert_response :ok
        assert_equal 200, json_body.dig("meta", "limit")
      end

      # acme_channel is granted github_token_inject and db_password_replace (see
      # grants.yml); give them sources so they materialize into the config.
      def grant_sources_to_acme_channel
        SecretSource.create!(source_type: "env", config: { "var" => "GITHUB_TOKEN" },
                             static_secret: static_secrets(:github_token_inject))
        SecretSource.create!(source_type: "control_plane", secret: "s3cr3t-db-pass",
                             static_secret: static_secrets(:db_password_replace))
      end

      test "GET effective_config returns the principal's resolved config" do
        grant_sources_to_acme_channel
        principal = principals(:acme_channel)

        get effective_config_api_v1_principal_url(id: principal.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal principal.oid, data["id"]
        assert_equal 2, data.fetch("secrets").length
        assert_kind_of Array, data.fetch("transforms")
        assert_kind_of Array, data.fetch("postgres")
      end

      test "GET effective_config redacts inline control_plane secret values" do
        grant_sources_to_acme_channel

        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid), headers: auth_headers
        assert_response :ok

        entry = json_body.dig("data", "secrets").find { |s| s.dig("source", "type") == "control_plane" }
        refute_nil entry
        assert_equal "[redacted]", entry.dig("source", "value")
        # A reference-style source passes through unredacted.
        env = json_body.dig("data", "secrets").find { |s| s.dig("source", "type") == "env" }
        assert_equal "GITHUB_TOKEN", env.dig("source", "var")
      end

      test "GET effective_config omits the config_hash" do
        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid), headers: auth_headers
        assert_response :ok
        refute json_body.fetch("data").key?("config_hash")
      end

      test "GET effective_config sends an ETag and forbids caching" do
        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid), headers: auth_headers
        assert_response :ok
        assert_match(/\A"[0-9a-f]{64}"\z/, response.headers["ETag"])
        assert_equal "no-store", response.headers["Cache-Control"]
      end

      test "GET effective_config resolves a foreign_id via the lookup route" do
        grant_sources_to_acme_channel
        principal = principals(:acme_channel)

        get lookup_effective_config_api_v1_principals_url(foreign_id: principal.foreign_id), headers: auth_headers
        assert_response :ok
        assert_equal principal.oid, json_body.dig("data", "id")
        assert_equal 2, json_body.dig("data", "secrets").length
      end

      test "GET effective_config rejects a non-default compatibility path" do
        principal = principals(:acme_channel)
        get "/api/v1/principals/lookup/other/#{principal.foreign_id}/effective_config", headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config does not resolve a foreign_id passed as :id" do
        principal = principals(:acme_channel)
        get effective_config_api_v1_principal_url(id: principal.foreign_id), headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config returns 404 for an unknown oid" do
        get effective_config_api_v1_principal_url(id: "prn_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config lookup returns 404 for an unknown foreign_id" do
        get lookup_effective_config_api_v1_principals_url(foreign_id: "U-does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET effective_config rejects unauthenticated requests" do
        get effective_config_api_v1_principal_url(id: principals(:acme_channel).oid)
        assert_response :unauthorized
      end
    end
  end
end
