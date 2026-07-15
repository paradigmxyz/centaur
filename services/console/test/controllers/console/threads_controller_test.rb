require "test_helper"
require "tmpdir"

class Console::ThreadsControllerTest < ActionDispatch::IntegrationTest
  TranscriptMessage = Struct.new(:role, :parts_array, :metadata_hash, :created_at, keyword_init: true)
  TranscriptSession = Struct.new(:metadata_hash, :harness_type, :title, keyword_init: true)
  ModelSession = Struct.new(:thread_key, :metadata_hash, :harness_type, keyword_init: true)
  ModelExecution = Struct.new(:metadata, keyword_init: true)
  TranscriptEvent = Struct.new(:event_type, :payload_hash, :created_at, keyword_init: true)
  SelectedSession = Struct.new(:thread_key, keyword_init: true)

  setup do
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "an admin sees the Control and Data Sync nav items" do
    with_recent_first_error do
      get console_threads_url
    end

    assert_response :ok
    assert_select ".console-nav-link", text: "Control"
    assert_select ".console-nav-link", text: "Data Sync"
  end

  test "a non-admin sees only the Integrations nav item, not Control or Data Sync" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    with_recent_first_error do
      get console_threads_url
    end

    assert_response :ok
    assert_select ".console-nav-link", count: 1, text: /Integrations/
    assert_select ".console-thread-group-title", text: /Chats/
  end

  test "threads page falls back to the new chat screen when session database is unavailable" do
    with_recent_first_error do
      get console_threads_url
    end

    assert_response :ok
    # No chat selected: the new-chat composer renders (posting goes through
    # the API, not the sessions DB), alongside the unavailability note.
    assert_select ".console-thread-detail-header", count: 0
    assert_select "a[aria-label=?]", "New chat", count: 1
    assert_select "textarea[name=prompt]", count: 1
    assert_select "body", text: /Chat database is unavailable/
  end

  test "plain threads page redirects to first visible thread" do
    skip_unless_session_table

    thread_key = "console:auto-select-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)

    get console_threads_url

    assert_redirected_to console_threads_path(thread: thread_key)
  end

  test "direct selected thread renders chat not found when the current user did not start it" do
    skip_unless_session_table

    thread_key = "slack:C0DIRECT:#{SecureRandom.hex(6)}"
    insert_slack_session(
      thread_key,
      slack_user_id: "U_OTHER",
      slack_user_name: "someone-else"
    )

    # @operator has no Slack OAuth credential matching U_OTHER, so this thread is
    # outside their owner scope. A direct ?thread= link must render a 404 chat
    # not found state instead of surfacing it or falling back to another chat.
    get console_threads_url(thread: thread_key)

    assert_response :not_found
    assert_select "body", text: /Chat not found/
    # The not-found rendering carries no page header and no explainer copy —
    # just the centered "Chat not found" state.
    assert_select ".console-thread-detail-header", count: 0
    assert_select "body", text: /may not exist/, count: 0
    assert_select "[data-thread-panel]", count: 0
    assert_select ".console-thread-list a.console-thread-link-active[href=?]",
                  console_threads_path(thread: thread_key),
                  count: 0
  end

  test "direct link to a nonexistent thread renders chat not found" do
    skip_unless_session_table

    # Even with an owned chat present, a bogus key must 404 rather than fall
    # back to the first visible chat.
    insert_console_session("console:owned-#{SecureRandom.hex(6)}")

    get console_threads_url(thread: "console:missing-#{SecureRandom.hex(6)}")

    assert_response :not_found
    assert_select "body", text: /Chat not found/
  end

  test "public Slack channel threads are readable by every console user only when enabled" do
    skip_unless_session_table
    skip_unless_slack_channel_table

    public_channel_id = "C#{SecureRandom.hex(6).upcase}"
    private_channel_id = "C#{SecureRandom.hex(6).upcase}"
    removed_channel_id = "C#{SecureRandom.hex(6).upcase}"
    public_thread_key = "slack:#{public_channel_id}:#{SecureRandom.hex(6)}"
    private_thread_key = "slack:#{private_channel_id}:#{SecureRandom.hex(6)}"
    removed_thread_key = "slack:#{removed_channel_id}:#{SecureRandom.hex(6)}"
    insert_slack_sync_channel(public_channel_id, is_private: false)
    insert_slack_sync_channel(private_channel_id, is_private: true)
    insert_slack_sync_channel(removed_channel_id, is_private: false, is_syncable: false)
    insert_slack_session(public_thread_key, slack_user_id: "U_OTHER", slack_user_name: "someone-else")
    insert_slack_session(private_thread_key, slack_user_id: "U_OTHER", slack_user_name: "someone-else")
    insert_slack_session(removed_thread_key, slack_user_id: "U_OTHER", slack_user_name: "someone-else")

    with_env(
      "CENTAUR_CONSOLE_PUBLIC_SLACK_THREADS_ENABLED" => nil,
      "IRON_CONTROL_PUBLIC_SLACK_THREADS_ENABLED" => nil
    ) do
      get console_threads_url(thread: public_thread_key)
      assert_response :not_found
    end

    with_env("CENTAUR_CONSOLE_PUBLIC_SLACK_THREADS_ENABLED" => "true") do
      get console_threads_url(thread: public_thread_key)
      assert_response :ok
      assert_select ".console-thread-detail-header", count: 1
      assert_select "textarea[name=prompt]", count: 0

      get console_threads_url(thread: private_thread_key)
      assert_response :not_found

      get console_threads_url(thread: removed_thread_key)
      assert_response :not_found
    end
  end

  test "sharing publishes a direct read-only link from an in-page copy dialog" do
    skip_unless_session_table

    thread_key = "console:shared-#{SecureRandom.hex(6)}"
    insert_console_session(thread_key)

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "button.console-thread-share-trigger[aria-label=?][data-action=?]",
                  "Share chat", "thread-share#open", count: 1 do
      assert_select "svg", count: 1
    end
    assert_select ".console-thread-menu", count: 0
    assert_select "button[data-turbo-confirm]", count: 0
    assert_select "dialog.console-share-dialog[data-thread-share-target=dialog]" do
      assert_select "h2", text: "Share chat"
      assert_select "p", text: "Anyone with access to Centaur Console will be able to view this chat."
      assert_select "form[action=?][data-action*=?]", console_thread_share_path, "thread-share#copyLink" do
        assert_select "input[name=thread_key][value=?]", thread_key
        assert_select "button.btn-secondary[type=button]", text: "Cancel"
        assert_select "button.btn-primary[type=submit]", text: "Copy link"
      end
    end

    post console_thread_share_url, params: { thread_key: thread_key }, as: :json

    assert_response :ok
    assert_equal console_threads_url(thread: thread_key), response.parsed_body.fetch("url")
    assert_equal @operator, ThreadShare.find_by!(thread_key: thread_key).created_by

    post console_thread_share_url, params: { thread_key: thread_key }

    assert_redirected_to console_threads_path(thread: thread_key)
    assert_nil flash[:notice]
    assert_equal 1, ThreadShare.where(thread_key: thread_key).count

    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select ".console-thread-detail-header", count: 1
    assert_select "textarea[name=prompt]", count: 0
  end

  test "a user cannot share a chat they cannot read" do
    skip_unless_session_table

    thread_key = "slack:G0PRIVATE12:#{SecureRandom.hex(6)}"
    insert_slack_session(thread_key, slack_user_id: "U_OTHER", slack_user_name: "someone-else")

    post console_thread_share_url, params: { thread_key: thread_key }

    assert_redirected_to console_threads_path
    assert_equal "Chat not found.", flash[:alert]
    assert_not ThreadShare.exists?(thread_key: thread_key)
  end

  test "a non-owner cannot persistently share a deployment-public Slack thread" do
    skip_unless_session_table
    skip_unless_slack_channel_table

    channel_id = "C#{SecureRandom.hex(6).upcase}"
    thread_key = "slack:#{channel_id}:#{SecureRandom.hex(6)}"
    insert_slack_sync_channel(channel_id, is_private: false)
    insert_slack_session(thread_key, slack_user_id: "U_OTHER", slack_user_name: "someone-else")

    with_env("CENTAUR_CONSOLE_PUBLIC_SLACK_THREADS_ENABLED" => "true") do
      post console_thread_share_url, params: { thread_key: thread_key }
    end

    assert_redirected_to console_threads_path
    assert_equal "Chat not found.", flash[:alert]
    assert_not ThreadShare.exists?(thread_key: thread_key)
  end

  test "slack assistant-role messages from the current Slack user render as user authored" do
    controller = Console::ThreadsController.new
    controller.define_singleton_method(:current_slack_user_ids) { [ "u123" ] }
    controller.instance_variable_set(
      :@selected_session,
      TranscriptSession.new(
        metadata_hash: {
          "slack_user_id" => "U123",
          "slack_display_name" => "Goksu Toprak",
          "slack_user_name" => "goksu"
        }
      )
    )
    message = TranscriptMessage.new(
      role: "assistant",
      parts_array: [ { "type" => "text", "text" => "Root Slack bot post" } ],
      metadata_hash: {
        "source" => "slackbotv2",
        "platform" => "slack",
        "slack_user_id" => "U123",
        "slack_display_name" => "U123"
      },
      created_at: Time.zone.parse("2026-06-26 17:15:58 UTC")
    )

    item = controller.send(:transcript_item_for_message, message)

    assert_equal "assistant", item[:role]
    assert_equal "Goksu Toprak", item[:label]
    assert_equal :end, item[:align]
    assert_equal "Root Slack bot post", item[:text]
  end

  test "slack message text resolves mentions from bot identity and selected actor metadata" do
    controller = Console::ThreadsController.new
    controller.define_singleton_method(:current_slack_user_ids) { [ "u123" ] }
    controller.instance_variable_set(
      :@selected_session,
      TranscriptSession.new(
        metadata_hash: {
          "slack_user_id" => "U123",
          "slack_display_name" => "Goksu Toprak",
          "slack_user_name" => "goksu"
        }
      )
    )
    message = TranscriptMessage.new(
      role: "user",
      parts_array: [
        {
          "type" => "text",
          "text" => "@UBOT Are you working? Also loop in <@U123>."
        }
      ],
      metadata_hash: {
        "source" => "slackbotv2",
        "platform" => "slack",
        "is_mention" => true,
        "slack_user_id" => "U123",
        "slack_display_name" => "Goksu Toprak",
        "slack_user_name" => "goksu"
      },
      created_at: Time.zone.parse("2026-06-26 17:15:58 UTC")
    )
    controller.instance_variable_set(:@selected_messages, [ message ])
    controller.instance_variable_set(:@selected_events, [])

    item = controller.send(:transcript_item_for_message, message)

    assert_equal "@ai Are you working? Also loop in @goksu.", item[:text]
  end

  test "slack mention resolution prefers synced user names when available" do
    controller = Console::ThreadsController.new
    controller.define_singleton_method(:current_slack_user_ids) { [] }
    controller.define_singleton_method(:slack_user_display_labels_from_database) do |_user_ids|
      { "u456" => "@alice" }
    end
    message = TranscriptMessage.new(
      role: "user",
      parts_array: [ { "type" => "text", "text" => "cc @U456" } ],
      metadata_hash: {
        "source" => "slackbotv2",
        "platform" => "slack",
        "slack_user_id" => "U123"
      },
      created_at: Time.zone.parse("2026-06-26 17:15:58 UTC")
    )
    controller.instance_variable_set(:@selected_session, TranscriptSession.new(metadata_hash: {}))
    controller.instance_variable_set(:@selected_messages, [ message ])
    controller.instance_variable_set(:@selected_events, [])

    item = controller.send(:transcript_item_for_message, message)

    assert_equal "cc @alice", item[:text]
  end

  test "slack messages from other actors keep their author label" do
    controller = Console::ThreadsController.new
    controller.define_singleton_method(:current_slack_user_ids) { [ "u123" ] }
    controller.instance_variable_set(
      :@selected_session,
      TranscriptSession.new(metadata_hash: { "slack_user_id" => "U123" })
    )
    message = TranscriptMessage.new(
      role: "user",
      parts_array: [ { "type" => "text", "text" => "Another person replied" } ],
      metadata_hash: {
        "source" => "slackbotv2",
        "platform" => "slack",
        "slack_user_id" => "U456",
        "slack_display_name" => "Alice"
      },
      created_at: Time.zone.parse("2026-06-26 17:15:58 UTC")
    )

    item = controller.send(:transcript_item_for_message, message)

    assert_equal "Alice", item[:label]
    assert_equal :start, item[:align]
  end

  test "slack messages from selected thread owner still show author when not current Slack user" do
    controller = Console::ThreadsController.new
    controller.define_singleton_method(:current_slack_user_ids) { [ "u999" ] }
    controller.define_singleton_method(:slack_mention_labels_by_id) { { "u123" => "@goksu" } }
    controller.instance_variable_set(
      :@selected_session,
      TranscriptSession.new(
        metadata_hash: {
          "slack_user_id" => "U123",
          "slack_display_name" => "Goksu Toprak",
          "slack_user_name" => "goksu"
        }
      )
    )
    message = TranscriptMessage.new(
      role: "user",
      parts_array: [ { "type" => "text", "text" => "Owner message in a direct linked thread" } ],
      metadata_hash: {
        "source" => "slackbotv2",
        "platform" => "slack",
        "slack_user_id" => "U123",
        "slack_display_name" => "U123"
      },
      created_at: Time.zone.parse("2026-06-26 17:15:58 UTC")
    )

    item = controller.send(:transcript_item_for_message, message)

    assert_equal "@goksu", item[:label]
    assert_equal :start, item[:align]
  end

  test "slack bot messages use configured bot username as author label" do
    controller = Console::ThreadsController.new
    controller.define_singleton_method(:current_slack_user_ids) { [] }
    mention = TranscriptMessage.new(
      role: "user",
      parts_array: [ { "type" => "text", "text" => "@UBOT Please check this." } ],
      metadata_hash: {
        "source" => "slackbotv2",
        "platform" => "slack",
        "is_mention" => true,
        "slack_user_id" => "U123"
      },
      created_at: Time.zone.parse("2026-06-26 17:15:58 UTC")
    )
    bot_message = TranscriptMessage.new(
      role: "user",
      parts_array: [ { "type" => "text", "text" => "Working on it." } ],
      metadata_hash: {
        "source" => "slackbotv2",
        "platform" => "slack",
        "slack_user_id" => "UBOT",
        "slack_display_name" => "UBOT"
      },
      created_at: Time.zone.parse("2026-06-26 17:16:58 UTC")
    )
    controller.instance_variable_set(:@selected_session, TranscriptSession.new(metadata_hash: {}))
    controller.instance_variable_set(:@selected_messages, [ mention, bot_message ])
    controller.instance_variable_set(:@selected_events, [])

    item = controller.send(:transcript_item_for_message, bot_message)

    assert_equal "@ai", item[:label]
    assert_equal :start, item[:align]
  end

  test "terminal execution events render as bot output" do
    controller = Console::ThreadsController.new
    event = TranscriptEvent.new(
      event_type: "session.execution_completed",
      payload_hash: { "result_text" => "The issue is real for @U123." },
      created_at: Time.zone.parse("2026-06-26 17:16:44 UTC")
    )
    controller.define_singleton_method(:slack_user_display_labels_from_database) do |_user_ids|
      { "u123" => "@goksu" }
    end
    controller.instance_variable_set(:@selected_session, TranscriptSession.new(metadata_hash: {}))
    controller.instance_variable_set(:@selected_messages, [])
    controller.instance_variable_set(:@selected_events, [ event ])

    item = controller.send(:transcript_item_for_event, event)

    assert_equal "assistant", item[:role]
    assert_equal "@ai", item[:label]
    assert_equal :start, item[:align]
    assert_equal "The issue is real for @goksu.", item[:text]
  end

  test "generated thread title strips slack mentions and clips to assistant title length" do
    controller = Console::ThreadsController.new
    title = controller.send(
      :generated_thread_title,
      "@U0ANX3AM5RR Approach truth-seeking to max and let me know if this is actually " \
        "a legit issue with extra context that should not fit"
    )

    assert_not_includes title, "@U0ANX3AM5RR"
    assert title.start_with?("Approach truth-seeking")
    assert_operator title.length, :<=, 80
    assert title.end_with?("...")
  end

  test "thread title prefers the stored generated title over metadata" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "summary" => { "title" => "metadata title" } },
      harness_type: "codex",
      title: "Fix worker memory leak"
    )

    assert_equal "Fix worker memory leak", controller.send(:thread_title, session)
  end

  test "thread title ignores a blank stored title" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "subject" => "Fallback subject" },
      harness_type: "codex",
      title: "  "
    )

    assert_equal "Fallback subject", controller.send(:thread_title, session)
  end

  test "thread title prefers stored summary metadata when present" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "summary" => { "title" => "Investigate rollout failure" } },
      harness_type: "codex"
    )

    assert_equal "Investigate rollout failure", controller.send(:thread_title, session)
  end

  test "thread title tolerates a plain string summary without raising" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "summary" => "a plain string" },
      harness_type: "codex"
    )

    assert_nothing_raised do
      assert_equal "a plain string", controller.send(:thread_title, session)
    end
  end

  test "thread title tolerates a string thread metadata without raising" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "thread" => "x", "subject" => "Fallback subject" },
      harness_type: "codex"
    )

    assert_nothing_raised do
      assert_equal "Fallback subject", controller.send(:thread_title, session)
    end
  end

  test "thread source and harness labels are display cased" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "platform" => "slack" },
      harness_type: "codex"
    )

    assert_equal "Slack", controller.send(:thread_source_label, session)
    assert_equal "slack", controller.send(:thread_source_icon, session)
    assert_equal "Codex", controller.send(:thread_harness_label, session)
  end

  test "thread model label prefers the latest execution's recorded model override" do
    controller = Console::ThreadsController.new
    session = ModelSession.new(
      thread_key: "slack:C1:1",
      metadata_hash: {},
      harness_type: "claudecode"
    )
    execution = ModelExecution.new(metadata: { "model" => "claude-sonnet-4-6" })
    controller.instance_variable_set(:@latest_executions, { "slack:C1:1" => execution })

    assert_equal "CLAUDE-SONNET-4-6", controller.send(:thread_model_label, session)
  end

  test "thread model label reads session metadata before the harness default" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "model" => "claude-fable-5" },
      harness_type: "claudecode"
    )

    assert_equal "CLAUDE-FABLE-5", controller.send(:thread_model_label, session)
  end

  test "thread model label falls back to the deployment's model env override" do
    controller = Console::ThreadsController.new

    with_env("CLAUDE_MODEL" => "claude-fable-5", "CODEX_MODEL" => "gpt-6") do
      assert_equal "CLAUDE-FABLE-5", controller.send(
        :thread_model_label,
        TranscriptSession.new(metadata_hash: {}, harness_type: "claudecode")
      )
      assert_equal "GPT-6", controller.send(
        :thread_model_label,
        TranscriptSession.new(metadata_hash: {}, harness_type: "codex")
      )
    end
  end

  test "thread model label falls back to the models pinned in the harness config files" do
    controller = Console::ThreadsController.new

    Dir.mktmpdir do |dir|
      FileUtils.mkdir_p(File.join(dir, "claude"))
      FileUtils.mkdir_p(File.join(dir, "codex"))
      File.write(File.join(dir, "claude", "settings.json"), { model: "claude-baked-1" }.to_json)
      File.write(File.join(dir, "codex", "config.toml"), <<~TOML)
        model = "gpt-baked-1"
        model_reasoning_effort = "low"
      TOML

      with_env("CLAUDE_MODEL" => nil, "CODEX_MODEL" => nil, "CENTAUR_HARNESS_CONFIG_DIR" => dir) do
        assert_equal "CLAUDE-BAKED-1", controller.send(
          :thread_model_label,
          TranscriptSession.new(metadata_hash: {}, harness_type: "claudecode")
        )
        assert_equal "GPT-BAKED-1", controller.send(
          :thread_model_label,
          TranscriptSession.new(metadata_hash: {}, harness_type: "codex")
        )
      end
    end
  end

  test "thread model label is nil for harnesses without a fixed default" do
    controller = Console::ThreadsController.new

    assert_nil controller.send(
      :thread_model_label,
      TranscriptSession.new(metadata_hash: {}, harness_type: "amp")
    )
  end

  test "visible thread scope matches Slack threads owned by the current user's Slack OAuth record" do
    app = oauth_apps(:acme_slack)
    app.update!(client_secret: "slack-secret", labels: { "slack_team_id" => "T123" })
    create_slack_oauth_credential(
      app,
      subject: "UOWNER",
      email: @operator.email,
      labels: { "slack_team_id" => "T123" }
    )
    controller = threads_controller_for(@operator)

    sql = controller.send(:visible_thread_scope).to_sql

    assert_includes sql, "thread_key LIKE 'slack:%'"
    assert_includes sql, "metadata ->> 'slack_user_id'"
    assert_includes sql, "uowner"
    assert_includes sql, "split_part(thread_key, ':', 2)"
    assert_includes sql, "t123"
  end

  test "visible thread scope keeps current user's console threads without Slack OAuth" do
    controller = threads_controller_for(@operator)
    sql = controller.send(:visible_thread_scope).to_sql

    assert_includes sql, "thread_key LIKE 'console:%'"
    assert_includes sql, @operator.email
    refute_includes sql, "slack_user_id"
  end

  test "public Slack thread visibility defaults off and never expands the owner scope" do
    controller = threads_controller_for(@operator)

    with_env(
      "CENTAUR_CONSOLE_PUBLIC_SLACK_THREADS_ENABLED" => nil,
      "IRON_CONTROL_PUBLIC_SLACK_THREADS_ENABLED" => nil
    ) do
      refute_includes controller.send(:visible_thread_scope).to_sql, "slack_sync_channels"
    end

    with_env("CENTAUR_CONSOLE_PUBLIC_SLACK_THREADS_ENABLED" => "true") do
      if slack_channel_privacy_catalog_available?
        assert_includes controller.send(:visible_thread_scope).to_sql, "slack_sync_channels"
      end
      refute_includes controller.send(:owned_thread_scope).to_sql, "slack_sync_channels"
    end
  end

  test "public Slack visibility fails closed without the synchronized channel catalog" do
    connection = CentaurSession.connection
    replacement = ->(_table) { false }

    with_singleton_method(connection, :data_source_exists?, replacement) do
      assert_nil CentaurSession.public_slack_channel_sql
    end
  end

  test "visible thread scope matches Slack threads by user id when the credential has no team" do
    app = oauth_apps(:acme_slack)
    app.update!(client_secret: "slack-secret", labels: {})
    create_slack_oauth_credential(
      app,
      subject: "UOWNER",
      email: @operator.email,
      labels: {}
    )
    controller = threads_controller_for(@operator)

    sql = controller.send(:visible_thread_scope).to_sql

    # slackbotv2 threads carry no team (slack:CHANNEL:TS keys, no slack_team_id),
    # so a team-less credential still matches on slack_user_id alone; team scoping
    # is added only when the credential exposes a team.
    assert_includes sql, "thread_key LIKE 'slack:%'"
    assert_includes sql, "metadata ->> 'slack_user_id'"
    assert_includes sql, "uowner"
    refute_includes sql, "split_part(thread_key, ':', 2)"
  end

  test "visible thread scope matches Slack threads via the SSO identity without a broker credential" do
    UserIdentity.create!(
      user: @operator,
      provider: "slack",
      subject: "USSOONLY",
      email: @operator.email,
      email_verified: true
    )
    controller = threads_controller_for(@operator)

    sql = controller.send(:visible_thread_scope).to_sql

    # The Slack OIDC subject is the workspace user id, so signing in with
    # Slack is enough to own the threads slackbotv2 attributed to that id —
    # no broker credential required.
    assert_includes sql, "thread_key LIKE 'slack:%'"
    assert_includes sql, "metadata ->> 'slack_user_id'"
    assert_includes sql, "ussoonly"
    refute_includes sql, "split_part(thread_key, ':', 2)"
  end

  test "visible thread scope dedupes an SSO identity that matches its broker credential" do
    app = oauth_apps(:acme_slack)
    app.update!(client_secret: "slack-secret", labels: {})
    create_slack_oauth_credential(app, subject: "UOWNER", email: @operator.email, labels: {})
    UserIdentity.create!(
      user: @operator,
      provider: "slack",
      subject: "UOWNER",
      email: @operator.email,
      email_verified: true
    )
    controller = threads_controller_for(@operator)

    owners = controller.send(:slack_thread_owners_for_current_user)

    assert_equal [ "UOWNER" ], owners.map { |owner| owner.user_id.upcase }
  end

  test "sidebar thread scope matches Slack threads via the SSO identity without a broker credential" do
    UserIdentity.create!(
      user: @operator,
      provider: "slack",
      subject: "USSOONLY",
      email: @operator.email,
      email_verified: true
    )
    controller = threads_controller_for(@operator)

    sql = controller.send(:console_sidebar_visible_thread_scope).to_sql

    assert_includes sql, "thread_key LIKE 'slack:%'"
    assert_includes sql, "metadata ->> 'slack_user_id'"
    assert_includes sql, "ussoonly"
  end

  test "sidebar includes public Slack threads only when the deploy setting is enabled" do
    controller = threads_controller_for(@operator)

    with_env("CENTAUR_CONSOLE_PUBLIC_SLACK_THREADS_ENABLED" => "true") do
      sql = controller.send(:console_sidebar_visible_thread_scope).to_sql

      if slack_channel_privacy_catalog_available?
        assert_includes sql, "slack_sync_channels"
      end
    end
  end

  test "selected session resolves a directly linked thread only within the owner scope" do
    controller = Console::ThreadsController.new
    owned_thread = SelectedSession.new(thread_key: "slack:C123:1782339173.755169")
    scoped_relation = Object.new
    scoped_relation.define_singleton_method(:where) do |thread_key:|
      thread_key == owned_thread.thread_key ? [ owned_thread ] : []
    end
    controller.instance_variable_set(:@starting_new_thread, false)
    controller.instance_variable_set(:@sessions, [])

    # An owned key outside the base window is recovered through the scope.
    controller.instance_variable_set(:@selected_thread_key, owned_thread.thread_key)
    assert_equal owned_thread, controller.send(:selected_session, scoped_relation, [])

    # A key the scope does not own has no unscoped fallback, so it stays hidden.
    controller.instance_variable_set(:@selected_thread_key, "slack:C999:1782339173.999999")
    assert_nil controller.send(:selected_session, scoped_relation, [])
  end

  test "renders the sidebar New chat link and the full-page composer" do
    with_composer do
      with_recent_first_error do
        get console_threads_url(new: 1)
      end
    end

    assert_response :ok
    assert_select "a[aria-label=?]", "New chat", count: 1
    assert_select "form[action=?]", console_threads_path do
      assert_select "textarea[name=prompt]", count: 1
      # The model picker is a custom menu (account-dropdown style) posting
      # through a hidden field, not a native select.
      assert_select "input[type=hidden][name=model]", count: 1
      assert_select "[data-console-model-option][data-value=?]", "amp"
      assert_select "select", count: 0
    end
    # Submitting replaces the centered empty state with a full-height,
    # bottom-aligned optimistic transcript while the request is in flight.
    assert_includes response.body, 'container.classList.add("console-new-chat--optimistic")'
    assert_includes response.body, ".console-new-chat--optimistic"
  end

  test "shows the new chat screen when nothing is selected" do
    with_composer do
      with_recent_first_error do
        get console_threads_url
      end
    end

    assert_response :ok
    assert_select "textarea[name=prompt]", count: 1
    assert_select "body", text: /No chats yet/, count: 0
  end

  test "an active execution renders a thinking indicator" do
    skip_unless_session_table
    insert_console_session("console:thinking-active")
    insert_session_execution("console:thinking-active", status: "running")

    get console_threads_url(thread: "console:thinking-active")

    assert_response :ok
    assert_select "[data-console-thinking-indicator]", count: 1
  end

  test "a completed execution renders no thinking indicator" do
    skip_unless_session_table
    insert_console_session("console:thinking-done")
    insert_session_execution("console:thinking-done", status: "completed")

    get console_threads_url(thread: "console:thinking-done")

    assert_response :ok
    assert_select "[data-console-thinking-indicator]", count: 0
  end

  test "an active thread wires a per-panel poller instead of a full-page refresh" do
    skip_unless_session_table
    thread_key = "console:poller-active-#{SecureRandom.hex(6)}"
    insert_console_session(thread_key)
    insert_session_execution(thread_key, status: "running")

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "[data-controller=thread-poller][data-thread-poller-active-value=true]", count: 1
    assert_select "[data-thread-poller-url-value=?]",
                  console_thread_panel_path(thread_key: thread_key),
                  count: 1
    # The old behavior re-rendered the whole console with a Turbo visit while
    # any pane was executing; that script must stay gone.
    assert_no_match "Turbo.visit(window.location.href", response.body
  end

  test "panel poll renders one thread's transcript with the active header" do
    skip_unless_session_table
    thread_key = "console:poller-panel-#{SecureRandom.hex(6)}"
    insert_console_session(thread_key)
    insert_session_message(thread_key, index: 1)
    insert_session_execution(thread_key, status: "running")

    get console_thread_panel_url(thread_key: thread_key)

    assert_response :ok
    assert_equal "true", response.headers["X-Console-Execution-Active"]
    assert_select "[data-console-thinking-indicator]", count: 1
    assert_match "message 1", response.body
    # Transcript stream only: no layout, no composer, no panel chrome.
    assert_select "textarea[name=prompt]", count: 0
    assert_select "[data-thread-panel]", count: 0
  end

  test "panel poll reports inactive once the execution completes" do
    skip_unless_session_table
    thread_key = "console:poller-done-#{SecureRandom.hex(6)}"
    insert_console_session(thread_key)
    insert_session_execution(thread_key, status: "completed")

    get console_thread_panel_url(thread_key: thread_key)

    assert_response :ok
    assert_equal "false", response.headers["X-Console-Execution-Active"]
    assert_select "[data-console-thinking-indicator]", count: 0
  end

  test "panel poll is scoped to threads the current user can read" do
    skip_unless_session_table
    thread_key = "slack:C0POLL:#{SecureRandom.hex(6)}"
    insert_slack_session(thread_key, slack_user_id: "U_OTHER", slack_user_name: "someone-else")

    get console_thread_panel_url(thread_key: thread_key)

    assert_response :not_found
  end

  test "a new sentinel pane opens a composer panel alongside a thread" do
    skip_unless_session_table
    insert_console_session("console:with-new-pane")

    with_composer do
      get console_threads_url(thread: "console:with-new-pane,new")
    end

    assert_response :ok
    assert_select "[data-thread-panel]", count: 2
    assert_select "[data-thread-panel=new]", count: 1
    assert_select "[data-thread-panel=new] textarea[name=prompt]", count: 1
    assert_select "[data-thread-panel=new] [data-console-model-picker]", count: 1
  end

  test "the new sentinel alone renders the full-page new chat screen" do
    with_composer do
      with_recent_first_error do
        get console_threads_url(thread: "new")
      end
    end

    assert_response :ok
    assert_select "[data-thread-panel]", count: 0
    assert_select "textarea[name=prompt]", count: 1
  end

  test "starting a chat from a pane swaps the sentinel for the created thread" do
    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url,
           params: {
             prompt: "Reply with PONG.",
             model: "gpt-5.5",
             open_threads: "console:other,new"
           }
    end

    thread_key = client.calls[0].last[:thread_key]
    assert_redirected_to console_threads_path(thread: "console:other,#{thread_key}")
  end

  test "renders a follow-up composer on an open chat" do
    skip_unless_session_table
    insert_console_session("console:composer-open")

    with_composer do
      get console_threads_url(thread: "console:composer-open")
    end

    assert_response :ok
    assert_select "form[action=?]", console_threads_path do
      assert_select "input[type=hidden][name=thread_key][value=?]", "console:composer-open"
      assert_select "textarea[name=prompt]", count: 1
      # Follow-ups stay on the chat's existing harness/model: no picker.
      assert_select "[data-console-model-picker]", count: 0
    end
    # Optimistic rendering must not clear the textarea until Turbo has copied
    # its value into FormData, or the controller receives a blank prompt.
    assert_includes response.body, 'form.addEventListener("formdata"'
  end

  test "starting a chat creates a session, appends the prompt, and executes it" do
    @operator.update!(name: "Ada Admin")
    UserIdentity.create!(user: @operator, provider: "slack", subject: "UADA")
    client = RecordingApiClient.new
    identity = SlackRequesterIdentity::Result.new(
      handle: "@ada", source: 'Slack profile custom field "GitHub"', reason: nil
    )
    test_case = self
    with_singleton_method(SlackRequesterIdentity, :resolve, ->(user_ids:) {
      test_case.assert_includes user_ids, "uada"
      identity
    }) do
      with_composer(client: client) do
        post console_threads_url,
             params: { prompt: "Reply with PONG.", model: "claude-opus-4-8" }
      end
    end

    assert_equal %i[create_session append_session_messages execute_session], client.calls.map(&:first)

    create = client.calls[0].last
    assert create[:thread_key].start_with?("console:"), "expected a console:-namespaced thread key"
    assert_equal "claudecode", create[:harness_type]
    assert_equal "console", create[:metadata][:platform]
    assert_equal "console", create[:metadata][:source]
    assert_equal @operator.email, create[:metadata][:actor_email]
    assert_equal "claude-opus-4-8", create[:metadata][:model]

    append = client.calls[1].last
    assert_equal create[:thread_key], append[:thread_key]
    message = append[:messages].first
    assert_equal "user", message[:role]
    assert_equal "Reply with PONG.", message[:parts].first[:text]
    assert_equal @operator.email, message[:metadata][:user_email]

    execute = client.calls[2].last
    assert_equal create[:thread_key], execute[:thread_key]
    assert execute[:idempotency_key].present?
    assert_equal "claude-opus-4-8", execute[:metadata][:model]
    line = JSON.parse(execute[:input_lines].first)
    assert_equal "user", line["type"]
    assert_equal create[:thread_key], line["thread_key"]
    assert_equal "claude-opus-4-8", line["model"]
    assert_equal message[:client_message_id], line["client_user_message_id"]
    requester_context = line.dig("message", "content", 0, "text")
    assert_includes requester_context, "# Requester Context"
    assert_includes requester_context, "Prompted by: @ada"
    assert_includes requester_context, 'GitHub handle source: Slack profile custom field "GitHub"'
    assert_includes requester_context, "GitHub handle verified: yes"
    assert_equal "Reply with PONG.", line.dig("message", "content", 1, "text")

    assert_redirected_to console_threads_path(thread: create[:thread_key])
  end

  test "picking Amp starts an amp chat and sends no model" do
    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url, params: { prompt: "Reply with PONG.", model: "amp" }
    end

    create = client.calls[0].last
    assert_equal "amp", create[:harness_type]
    assert_not create[:metadata].key?(:model)

    execute = client.calls[2].last
    assert_not execute[:metadata].key?(:model)
    line = JSON.parse(execute[:input_lines].first)
    assert_not line.key?("model")
  end

  test "starting a chat with an unknown model is rejected" do
    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url, params: { prompt: "Reply with PONG.", model: "hal9000" }
    end

    assert_empty client.calls
    assert_redirected_to console_threads_path(new: 1)
    assert_match(/Unknown model/, flash[:alert])
  end

  test "a gpt model pick starts a codex chat" do
    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url, params: { prompt: "Reply with PONG.", model: "gpt-5.5" }
    end

    create = client.calls[0].last
    assert_equal "codex", create[:harness_type]
    assert_equal "gpt-5.5", create[:metadata][:model]
  end

  test "a codex chat carries the picked reasoning effort" do
    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url,
           params: { prompt: "Reply with PONG.", model: "gpt-5.6-sol", effort: "max" }
    end

    execute = client.calls[2].last
    assert_equal "max", execute[:metadata][:reasoning]
    line = JSON.parse(execute[:input_lines].first)
    assert_equal "max", line["reasoning"]
  end

  test "an effort the model does not offer is dropped" do
    client = RecordingApiClient.new
    with_composer(client: client) do
      # max is 5.6-only; claude models take no effort at all.
      post console_threads_url,
           params: { prompt: "Reply with PONG.", model: "gpt-5.5", effort: "max" }
      post console_threads_url,
           params: { prompt: "Reply with PONG.", model: "claude-opus-4-8", effort: "high" }
    end

    [ 2, 5 ].each do |index|
      execute = client.calls[index].last
      assert_not execute[:metadata].key?(:reasoning)
      assert_not JSON.parse(execute[:input_lines].first).key?("reasoning")
    end
  end

  test "a blank prompt asks for a message" do
    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url, params: { prompt: "   " }
    end

    assert_empty client.calls
    assert_redirected_to console_threads_path(new: 1)
    assert_equal "Type a message first.", flash[:alert]
  end

  test "replying appends and executes on an owned chat without creating a session" do
    skip_unless_session_table
    insert_console_session("console:composer-reply")

    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url,
           params: {
             prompt: "Continue from here.",
             thread_key: "console:composer-reply",
             open_threads: "console:composer-reply,console:other"
           }
    end

    assert_equal %i[append_session_messages execute_session], client.calls.map(&:first)
    assert_equal "console:composer-reply", client.calls[0].last[:thread_key]
    assert_redirected_to console_threads_path(thread: "console:composer-reply,console:other")
  end

  test "replying into a chat outside the owner scope is rejected" do
    skip_unless_session_table

    client = RecordingApiClient.new
    with_composer(client: client) do
      post console_threads_url,
           params: { prompt: "Continue from here.", thread_key: "console:not-mine" }
    end

    assert_empty client.calls
    assert_redirected_to console_threads_path
    assert_equal "Chat not found.", flash[:alert]
  end

  test "a session api error surfaces as a flash alert" do
    client = RecordingApiClient.new(error: CentaurApiClient::Error.new("boom"))
    with_composer(client: client) do
      post console_threads_url, params: { prompt: "Reply with PONG.", harness_type: "codex" }
    end

    assert_redirected_to console_threads_path(new: 1)
    assert_match(/boom/, flash[:alert])
  end

  # Fix 6: the sidebar thread list is loaded lazily via a Turbo Frame so the
  # cross-database sessions query never runs during the primary page render.
  test "console pages defer the sidebar thread list to a lazy turbo frame" do
    # A non-thread page must not run the sessions query during its render: if it
    # did, load_console_sidebar_threads would be invoked. Track invocations and
    # assert none happen while rendering the primary page.
    original = ApplicationController.instance_method(:load_console_sidebar_threads)
    Thread.current[:sidebar_loaded] = false
    ApplicationController.send(:define_method, :load_console_sidebar_threads) do
      Thread.current[:sidebar_loaded] = true
      original.bind(self).call
    end

    begin
      get console_principals_url

      assert_response :ok
      assert_not Thread.current[:sidebar_loaded],
                 "primary page render must not load the sidebar thread list"
      assert_select "turbo-frame#console_sidebar_threads[src=?]", console_sidebar_threads_path
      assert_select "turbo-frame#console_sidebar_threads[loading=?]", "lazy"
    ensure
      ApplicationController.send(:define_method, :load_console_sidebar_threads, original)
      Thread.current[:sidebar_loaded] = nil
    end
  end

  test "sidebar action renders the empty thread list when the session DB is unavailable" do
    with_recent_first_error do
      get console_sidebar_threads_url
    end

    assert_response :ok
    assert_select "turbo-frame#console_sidebar_threads"
    assert_select ".console-thread-empty", text: /No recent chats/
  end

  # Fix 5: selected_messages must return the NEWEST MESSAGE_LIMIT messages, in
  # oldest-first display order. A previous ascending order + limit returned the
  # oldest N and dropped the newest for long threads.
  test "selected_messages query fetches newest messages first with a limit" do
    # Building the SQL type-casts against the session_messages schema, which
    # only exists where the api-rs session tables are present.
    skip_unless_session_table

    relation = CentaurSessionMessage
      .where(thread_key: "console:ordering")
      .order(created_at: :desc, message_id: :desc)
      .limit(Console::ThreadsController::MESSAGE_LIMIT)
    sql = relation.to_sql

    assert_match(/ORDER BY.*created_at.*DESC.*message_id.*DESC/i, sql)
    assert_match(/LIMIT #{Console::ThreadsController::MESSAGE_LIMIT}\b/, sql)
  end

  test "selected_messages returns newest messages in ascending display order" do
    skip_unless_session_table

    thread_key = "console:transcript-order"
    insert_console_session(thread_key)

    limit = Console::ThreadsController::MESSAGE_LIMIT
    total = limit + 5
    total.times do |i|
      insert_session_message(thread_key, index: i)
    end

    controller = Console::ThreadsController.new
    controller.instance_variable_set(:@selected_session, SelectedSession.new(thread_key: thread_key))

    messages = controller.send(:selected_messages)

    assert_equal limit, messages.size
    indices = messages.map { |m| m.message_id.split("-").last.to_i }
    # Oldest-first display order over the newest `limit` messages: the earliest
    # (index 0..4) are dropped, and what remains is ascending.
    assert_equal (total - limit...total).to_a, indices
    assert_equal indices, indices.sort
  end

  OutputLineEvent = Struct.new(:payload, :created_at, :execution_id, :event_id, keyword_init: true)

  test "thinking transcript item is extracted from a completed reasoning output line" do
    controller = Console::ThreadsController.new
    line = {
      method: "item/completed",
      params: {
        item: {
          type: "reasoning",
          content: [ "First I will check the schema.", "Then write the query." ]
        }
      }
    }.to_json
    event = OutputLineEvent.new(payload: line, created_at: Time.zone.parse("2026-06-26 17:15:58 UTC"))

    item = controller.send(:thinking_transcript_item, event)

    assert_equal "thinking", item[:role]
    assert_equal "Thinking", item[:label]
    assert_equal :thinking, item[:source]
    assert_equal :start, item[:align]
    assert_equal "First I will check the schema.\nThen write the query.", item[:text]
    assert_equal event.created_at, item[:created_at]
  end

  test "thinking extraction accepts dot-form types and summary-only reasoning" do
    controller = Console::ThreadsController.new
    line = {
      type: "item.completed",
      item: { type: "reasoning", summary: [ { text: "Condensed thought." } ] }
    }.to_json
    event = OutputLineEvent.new(payload: line, created_at: Time.zone.now)

    item = controller.send(:thinking_transcript_item, event)

    assert_equal "Condensed thought.", item[:text]
  end

  test "thinking extraction formats completed command execution output lines" do
    controller = Console::ThreadsController.new
    line = {
      method: "item/completed",
      params: {
        item: {
          id: "cmd-1",
          type: "commandExecution",
          command: "pnpm test",
          status: "completed",
          aggregatedOutput: "ok\n",
          exitCode: 0
        }
      }
    }.to_json
    event = OutputLineEvent.new(payload: line, created_at: Time.zone.parse("2026-06-26 17:15:58 UTC"))

    item = controller.send(:thinking_transcript_item, event)

    assert_equal "thinking", item[:role]
    assert_equal "Ran 1 command", item[:label]
    assert_equal :thinking, item[:source]
    assert_equal "command", item[:trace_kind]
    assert_equal 1, item[:commands].length
    assert_equal "pnpm test", item[:commands].first[:command]
    assert_equal "ok\n", item[:commands].first[:output]
    assert_equal 0, item[:commands].first[:exit_code]
    assert_not item[:commands].first[:failed]
    assert_includes item[:text], "Status: completed"
    assert_includes item[:text], "Exit code: 0"
    assert_includes item[:text], "```sh\npnpm test\n```"
    assert_includes item[:text], "Output:"
    assert_includes item[:text], "```text\nok\n```"
  end

  test "compact trace grouping combines adjacent command executions for one run" do
    controller = Console::ThreadsController.new
    now = Time.zone.now
    first = {
      role: "thinking",
      label: "Ran 1 command",
      text: "$ pnpm test",
      trace_kind: "command",
      commands: [ { command: "pnpm test", output: "ok\n", exit_code: 0, status: "completed", failed: false } ],
      execution_id: "exe-1",
      created_at: now,
      source: :thinking
    }
    second = {
      role: "thinking",
      label: "Ran 1 command",
      text: "$ curl bad",
      trace_kind: "command",
      commands: [ { command: "curl bad", output: "failed\n", exit_code: 22, status: "completed", failed: true } ],
      execution_id: "exe-1",
      created_at: now + 1.second,
      source: :thinking
    }
    thought = {
      role: "thinking",
      label: "Thinking",
      text: "Need one more check.",
      trace_kind: "thinking",
      created_at: now + 2.seconds,
      source: :thinking
    }

    grouped = controller.send(:compact_trace_items, [ first, second, thought ])

    assert_equal 2, grouped.length
    assert_equal "commands", grouped.first[:trace_kind]
    assert_equal "Ran 2 commands", grouped.first[:label]
    assert_equal "1 failed", grouped.first[:failed_label]
    assert_equal [ "pnpm test", "curl bad" ], grouped.first[:commands].map { |command| command[:command] }
    assert_equal thought, grouped.second
  end

  test "activity summaries attach to the latest trace item at or before their source line" do
    controller = Console::ThreadsController.new
    items = [
      { event_id: 10, text: "first" },
      { event_id: 20, text: "second" }
    ]
    summaries = [
      TranscriptEvent.new(event_type: "session.activity_summary", payload_hash: { "summary" => "I found the bug", "source_event_id" => 9 }),
      TranscriptEvent.new(event_type: "session.activity_summary", payload_hash: { "summary" => "I'm reading the schema", "source_event_id" => 11 }),
      TranscriptEvent.new(event_type: "session.activity_summary", payload_hash: { "summary" => "I'm writing the query", "source_event_id" => 15 }),
      TranscriptEvent.new(event_type: "session.activity_summary", payload_hash: { "summary" => "", "source_event_id" => 21 })
    ]
    controller.define_singleton_method(:selected_activity_summaries) { summaries }

    controller.send(:apply_activity_summaries, items)

    # The newest summary in an item's window wins; blank summaries and
    # summaries preceding every trace item are dropped.
    assert_equal "I'm writing the query", items[0][:summary]
    assert_nil items[1][:summary]
  end

  test "thinking extraction formats claude stream-json tool calls" do
    controller = Console::ThreadsController.new
    line = {
      type: "assistant",
      message: {
        content: [
          { type: "tool_use", id: "toolu_1", name: "websearch", input: { query: "centaur" } }
        ]
      }
    }.to_json
    event = OutputLineEvent.new(payload: line, created_at: Time.zone.now)

    item = controller.send(:thinking_transcript_item, event)

    assert_equal "Tool call", item[:label]
    assert_includes item[:text], "Use websearch"
    assert_includes item[:text], '"query": "centaur"'
  end

  test "thinking extraction ignores partial and unrelated output lines" do
    controller = Console::ThreadsController.new
    now = Time.zone.now

    delta = { method: "item/reasoning/textDelta", params: { delta: "partial" } }.to_json
    started_tool = {
      method: "item/started",
      params: { item: { type: "commandExecution", command: "pnpm test" } }
    }.to_json
    non_json = "plain stdout noise mentioning reasoning"
    non_string = { "result" => "reasoning" }

    assert_nil controller.send(:thinking_transcript_item, OutputLineEvent.new(payload: delta, created_at: now))
    assert_nil controller.send(:thinking_transcript_item, OutputLineEvent.new(payload: started_tool, created_at: now))
    assert_nil controller.send(:thinking_transcript_item, OutputLineEvent.new(payload: non_json, created_at: now))
    assert_nil controller.send(:thinking_transcript_item, OutputLineEvent.new(payload: non_string, created_at: now))
  end

  test "thinking transcript item is extracted from a claude stream-json assistant line" do
    controller = Console::ThreadsController.new
    line = {
      type: "assistant",
      message: {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "The schema mismatch explains the failure.", signature: "sig" },
          { type: "text", text: "Here is the fix." }
        ]
      }
    }.to_json
    event = OutputLineEvent.new(payload: line, created_at: Time.zone.parse("2026-06-26 17:15:58 UTC"))

    item = controller.send(:thinking_transcript_item, event)

    assert_equal "thinking", item[:role]
    assert_equal :thinking, item[:source]
    assert_equal "The schema mismatch explains the failure.", item[:text]
    assert_equal event.created_at, item[:created_at]
  end

  test "thinking extraction joins multiple claude thinking blocks and skips thinking-free assistant lines" do
    controller = Console::ThreadsController.new
    now = Time.zone.now

    multi = {
      type: "assistant",
      message: {
        content: [
          { type: "thinking", thinking: "First thought." },
          { type: "thinking", thinking: "Second thought." }
        ]
      }
    }.to_json
    text_only = {
      type: "assistant",
      message: { content: [ { type: "text", text: "No thinking here." } ] }
    }.to_json
    stream_event = {
      type: "stream_event",
      event: { delta: { type: "thinking_delta", thinking: "partial" } }
    }.to_json

    item = controller.send(:thinking_transcript_item, OutputLineEvent.new(payload: multi, created_at: now))
    assert_equal "First thought.\nSecond thought.", item[:text]
    assert_nil controller.send(:thinking_transcript_item, OutputLineEvent.new(payload: text_only, created_at: now))
    assert_nil controller.send(:thinking_transcript_item, OutputLineEvent.new(payload: stream_event, created_at: now))
  end

  test "requested thread keys are deduped, stripped, and capped at the panel limit" do
    controller = Console::ThreadsController.new
    controller.params = ActionController::Parameters.new(
      thread: " a , b,a,, c ,d,e "
    )

    assert_equal %w[a b c d], controller.send(:requested_thread_keys)
  end

  test "thinking trace renders as a collapsed disclosure in the transcript" do
    skip_unless_session_table

    thread_key = "console:thinking-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)
    insert_session_message(thread_key, index: 0)
    insert_reasoning_event(thread_key, text: "I should compare the two schemas before answering.")

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "details.console-thinking summary", text: /Thinking/
    assert_select "details.console-thinking", text: /compare the two schemas/
  end

  test "tool trace renders as a collapsed disclosure in the transcript" do
    skip_unless_session_table

    thread_key = "console:tool-trace-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)
    insert_session_message(thread_key, index: 0)
    insert_command_trace_event(thread_key, command: "pnpm test", output: "ok\n")

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "details.console-thinking summary", text: /Ran 1 command/
    assert_select ".console-thinking-command-row", text: /pnpm test/
    assert_select ".console-thinking-command-full", text: /\$ pnpm test/
    assert_select ".console-thinking-command-result", text: /ok/
    assert_select ".console-thinking-command-meta", count: 0
    assert_select "details.console-thinking", text: /Status:/, count: 0
    assert_select "details.console-thinking", text: /pnpm test/
    assert_select "details.console-thinking", text: /ok/
  end

  test "thinking preview shows the activity summary covering its block" do
    skip_unless_session_table

    thread_key = "console:activity-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)
    insert_session_message(thread_key, index: 0)
    source_event_id = insert_reasoning_event(thread_key, text: "I should compare the two schemas before answering.")
    insert_activity_summary_event(
      thread_key,
      summary: "I'm comparing the two schemas",
      source_event_id: source_event_id
    )

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "details.console-thinking .console-thinking-preview",
                  text: /I'm comparing the two schemas/
    # The full thinking text stays available in the disclosure body.
    assert_select "details.console-thinking", text: /compare the two schemas before answering/
  end

  test "command trace group shows the activity summary as its collapsed preview" do
    skip_unless_session_table

    thread_key = "console:activity-cmd-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)
    insert_session_message(thread_key, index: 0)
    source_event_id = insert_command_trace_event(thread_key, command: "pnpm test", output: "ok\n")
    insert_activity_summary_event(
      thread_key,
      summary: "I'm running the test suite",
      source_event_id: source_event_id
    )

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "details.console-thinking summary", text: /Ran 1 command/
    assert_select "details.console-thinking .console-thinking-preview",
                  text: /I'm running the test suite/
  end

  test "split view renders owned panes as panels and drops unowned keys" do
    skip_unless_session_table

    primary_key = "console:panel-a-#{SecureRandom.hex(6)}"
    pane_key = "console:panel-b-#{SecureRandom.hex(6)}"
    unowned_key = "slack:C0PANEL:#{SecureRandom.hex(6)}"
    insert_console_session(primary_key)
    insert_console_session(pane_key)
    insert_slack_session(unowned_key, slack_user_id: "U_OTHER", slack_user_name: "someone-else")

    get console_threads_url(thread: [ primary_key, pane_key, unowned_key ].join(","))

    assert_response :ok
    assert_select "[data-thread-panel]", count: 2
    assert_select "[data-thread-panel=?]", primary_key
    assert_select "[data-thread-panel=?]", pane_key
    assert_select "[data-thread-panel=?]", unowned_key, count: 0
    # Each panel exposes a close control back to the remaining threads.
    assert_select "[data-thread-panel] a[aria-label='Close panel']", count: 2
  end

  test "split view caps the grid at four panels" do
    skip_unless_session_table

    keys = Array.new(5) { |i| "console:panel-cap-#{i}-#{SecureRandom.hex(4)}" }
    keys.each { |key| insert_console_session(key) }

    get console_threads_url(thread: keys.join(","))

    assert_response :ok
    assert_select "[data-thread-panel]", count: Console::ThreadsController::PANEL_LIMIT
  end

  test "single thread view does not render the split grid" do
    skip_unless_session_table

    thread_key = "console:solo-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "[data-thread-panel]", count: 0
    # column-reverse scroll container opens the thread at its newest message.
    assert_select "#thread-transcript-scroll.console-transcript-scroll"
  end

  test "sidebar thread links carry the cmd-click split view hook" do
    skip_unless_session_table

    thread_key = "console:sidebar-split-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)

    get console_sidebar_threads_url

    assert_response :ok
    # The layout's Cmd/Ctrl-click handler targets this attribute to add the
    # thread to the split-view grid.
    assert_select "a[data-console-thread-link][href=?]",
                  console_threads_path(thread: thread_key)
  end

  # The sidebar list loads out of band via a lazy Turbo Frame, so the page must
  # forward the current thread selection on the frame src for the active
  # highlight to render.
  test "threads page forwards the thread selection to the sidebar frame src" do
    skip_unless_session_table

    thread_key = "console:sidebar-active-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select "turbo-frame#console_sidebar_threads[src=?]",
                  console_sidebar_threads_path(thread: thread_key)
  end

  test "sidebar highlights every open thread of a split view" do
    skip_unless_session_table

    keys = Array.new(2) { |i| "console:sidebar-open-#{i}-#{SecureRandom.hex(4)}" }
    keys.each { |key| insert_console_session(key) }

    get console_sidebar_threads_url(thread: keys.join(","))

    assert_response :ok
    # Open threads carry their 1-based pane number in grid order; no filled
    # pill on thread rows.
    assert_select "a.console-thread-link-open[data-console-pane-index='1'][href=?]",
                  console_threads_path(thread: keys.first)
    assert_select "a.console-thread-link-open[data-console-pane-index='2'][href=?]",
                  console_threads_path(thread: keys.last)
    assert_select "a.console-thread-link-active", count: 0
  end

  test "split view close control drops one thread and keeps the rest open" do
    skip_unless_session_table

    keys = Array.new(3) { |i| "console:panel-close-#{i}-#{SecureRandom.hex(4)}" }
    keys.each { |key| insert_console_session(key) }

    get console_threads_url(thread: keys.join(","))

    assert_response :ok
    # Closing the middle panel keeps the primary and the last pane.
    assert_select "[data-thread-panel=?] a[aria-label='Close panel'][href=?]",
                  keys[1],
                  console_threads_path(thread: [ keys[0], keys[2] ].join(","))
    # Closing the primary panel promotes the next thread to primary.
    assert_select "[data-thread-panel=?] a[aria-label='Close panel'][href=?]",
                  keys[0],
                  console_threads_path(thread: [ keys[1], keys[2] ].join(","))
  end

  private

  # Fake CentaurApiClient recording every composer call; raises `error` from
  # each method instead when given, to exercise the failure paths.
  class RecordingApiClient
    attr_reader :calls

    def initialize(error: nil)
      @calls = []
      @error = error
    end

    def create_session(**kwargs) = record(:create_session, kwargs)
    def append_session_messages(**kwargs) = record(:append_session_messages, kwargs)
    def execute_session(**kwargs) = record(:execute_session, kwargs)

    private

    def record(name, kwargs)
      raise @error if @error

      @calls << [ name, kwargs ]
      {}
    end
  end

  # Runs the block with the injected fake session client.
  def with_composer(client: RecordingApiClient.new)
    original_factory = Console::ThreadsController.client_factory
    Console::ThreadsController.client_factory = -> { client }
    yield client
  ensure
    Console::ThreadsController.client_factory = original_factory
  end

  # Sets each env var for the block (nil deletes) and restores the previous
  # values afterwards.
  def with_env(overrides)
    previous = overrides.keys.index_with { |name| ENV[name] }
    overrides.each { |name, value| value.nil? ? ENV.delete(name) : ENV[name] = value }
    yield
  ensure
    previous.each { |name, value| value.nil? ? ENV.delete(name) : ENV[name] = value }
  end

  def with_recent_first_error
    singleton = class << CentaurSession; self; end
    original = CentaurSession.method(:recent_first)
    singleton.define_method(:recent_first) { raise ActiveRecord::ConnectionNotEstablished }
    yield
  ensure
    singleton.define_method(:recent_first, original)
  end

  def threads_controller_for(user)
    Console::ThreadsController.new.tap do |controller|
      controller.define_singleton_method(:current_user) { user }
    end
  end

  def create_slack_oauth_credential(app, subject:, email:, labels: {})
    BrokerCredential.create!(
      namespace: app.credential_namespace,
      oauth_app: app,
      provider_subject: subject,
      provider_email: email,
      labels: labels,
      token_endpoint: app.provider_strategy.token_endpoint,
      refresh_token: "refresh-#{subject}",
      access_token: "access-#{subject}",
      expires_at: 1.hour.from_now,
      last_refresh: Time.current,
      external_user_key: "user-#{subject}"
    )
  end

  def insert_console_session(thread_key)
    connection = CentaurSession.connection
    metadata = { platform: "console", actor_email: @operator.email }.to_json
    insert_session(thread_key, metadata)
  end

  def skip_unless_session_table
    skip("api-rs session tables are unavailable") unless CentaurSession.connection.data_source_exists?("sessions")
  end

  def skip_unless_slack_channel_table
    return if slack_channel_privacy_catalog_available?

    skip("Slack channel privacy catalog is unavailable")
  end

  def slack_channel_privacy_catalog_available?
    return false unless CentaurSession.connection.data_source_exists?(:slack_sync_channels)

    %i[is_private is_syncable].all? do |column|
      CentaurSession.connection.column_exists?(:slack_sync_channels, column)
    end
  end

  def insert_slack_sync_channel(channel_id, is_private:, is_syncable: true)
    connection = CentaurSession.connection
    connection.execute(<<~SQL.squish)
      insert into slack_sync_channels (channel_id, channel_name, is_private, is_syncable)
      values (
        #{connection.quote(channel_id)},
        #{connection.quote(channel_id.downcase)},
        #{connection.quote(is_private)},
        #{connection.quote(is_syncable)}
      )
      on conflict (channel_id) do update set
        is_private = excluded.is_private,
        is_syncable = excluded.is_syncable
    SQL
  end

  def insert_slack_session(thread_key, slack_user_id:, slack_user_name:)
    metadata = {
      source: "slackbotv2",
      platform: "slack",
      thread_id: thread_key,
      slack_user_id: slack_user_id,
      slack_user_name: slack_user_name
    }.to_json
    insert_session(thread_key, metadata)
  end

  def insert_session_execution(thread_key, status:)
    connection = CentaurSession.connection
    connection.execute(<<~SQL.squish)
      insert into session_executions (execution_id, thread_key, status, metadata, created_at, updated_at)
      values (
        #{connection.quote("#{thread_key}-exec")},
        #{connection.quote(thread_key)},
        #{connection.quote(status)},
        '{}'::jsonb,
        now(),
        now()
      )
    SQL
  end

  def insert_session_message(thread_key, index:)
    connection = CentaurSession.connection
    parts = [ { type: "text", text: "message #{index}" } ].to_json
    connection.execute(<<~SQL.squish)
      insert into session_messages (message_id, thread_key, role, parts, metadata, created_at)
      values (
        #{connection.quote("#{thread_key}-msg-#{index}")},
        #{connection.quote(thread_key)},
        'user',
        #{connection.quote(parts)}::jsonb,
        '{}'::jsonb,
        now() + (#{index} * interval '1 second')
      )
    SQL
  end

  # Mirrors how api-rs persists harness stdout: the payload column is a
  # JSON-encoded *string* holding one protocol notification line.
  def insert_reasoning_event(thread_key, text:)
    insert_output_line_event(
      thread_key,
      method: "item/completed",
      params: { item: { type: "reasoning", content: [ text ] } }
    )
  end

  def insert_command_trace_event(thread_key, command:, output:)
    insert_output_line_event(
      thread_key,
      method: "item/completed",
      params: {
        item: {
          type: "commandExecution",
          command: command,
          status: "completed",
          aggregatedOutput: output,
          exitCode: 0
        }
      }
    )
  end

  def insert_output_line_event(thread_key, method:, params:)
    connection = CentaurSession.connection
    line = { method: method, params: params }.to_json
    connection.select_value(<<~SQL.squish).to_i
      insert into session_events (thread_key, event_type, payload, created_at)
      values (
        #{connection.quote(thread_key)},
        'session.output.line',
        #{connection.quote(line.to_json)}::jsonb,
        now()
      )
      returning event_id
    SQL
  end

  # Mirrors api-rs's activity-summary worker: the payload is a JSON object
  # whose source_event_id points at the output line that triggered it.
  def insert_activity_summary_event(thread_key, summary:, source_event_id:)
    connection = CentaurSession.connection
    payload = { summary: summary, source_event_id: source_event_id }.to_json
    connection.execute(<<~SQL.squish)
      insert into session_events (thread_key, event_type, payload, created_at)
      values (
        #{connection.quote(thread_key)},
        'session.activity_summary',
        #{connection.quote(payload)}::jsonb,
        now()
      )
    SQL
  end

  def insert_session(thread_key, metadata)
    connection = CentaurSession.connection
    connection.execute(<<~SQL.squish)
      insert into sessions (thread_key, harness_type, status, metadata, created_at, updated_at)
      values (
        #{connection.quote(thread_key)},
        'codex',
        'active',
        #{connection.quote(metadata)}::jsonb,
        now() + interval '1 day',
        now() + interval '1 day'
      )
    SQL
  end

  def with_singleton_method(object, method_name, replacement)
    singleton = object.singleton_class
    original = singleton.instance_method(method_name)
    singleton.define_method(method_name, replacement)
    yield
  ensure
    singleton.define_method(method_name, original)
  end
end
