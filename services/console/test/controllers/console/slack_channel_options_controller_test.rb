require "test_helper"

module Console
  class SlackChannelOptionsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "principal options return bounded catalog matches and exclude existing permissions" do
      principal = principals(:acme_channel)
      existing = principal.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
      captured = nil
      result = SlackChannelCatalog::Result.new(
        channels: [ SlackChannelCatalog::Channel.new(id: "C1111111111", name: "engineering", private: false) ],
        error: nil,
        configured: true
      )

      search = lambda do |**args|
        captured = args
        result
      end
      SlackChannelCatalogProvider.stub(:search, search) do
        get console_principal_slack_channel_options_url(principal.oid), params: { q: "eng" }
      end

      assert_response :ok
      assert_equal "no-store", response.headers.fetch("Cache-Control")
      assert_equal "eng", captured.fetch(:query)
      assert_equal 20, captured.fetch(:limit)
      assert_includes captured.fetch(:exclude_ids), existing.channel_id
      assert_equal(
        {
          "options" => [
            {
              "value" => "C1111111111",
              "label" => "#engineering",
              "description" => "C1111111111 · Public"
            }
          ],
          "error" => nil
        },
        response.parsed_body
      )
    end

    test "role options use the same lookup endpoint" do
      role = roles(:acme_infra)
      result = SlackChannelCatalog::Result.new(channels: [], error: nil, configured: true)

      SlackChannelCatalogProvider.stub(:search, result) do
        get slack_channel_options_console_role_url(role.oid)
      end

      assert_response :ok
      assert_equal [], response.parsed_body.fetch("options")
    end

    test "non-admins cannot search the catalog" do
      delete logout_url
      post login_url, params: { email: users(:member_user).email, password: "password123456" }

      get console_principal_slack_channel_options_url(principals(:acme_channel).oid)

      assert_redirected_to console_threads_path
    end
  end
end
