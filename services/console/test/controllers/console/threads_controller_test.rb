require "test_helper"

class Console::ThreadsControllerTest < ActionDispatch::IntegrationTest
  class RecordingClient
    attr_reader :calls

    def initialize
      @calls = []
    end

    def create_session(**kwargs)
      @calls << [ :create_session, kwargs ]
      { "thread_key" => kwargs[:thread_key] }
    end

    def append_session_messages(**kwargs)
      @calls << [ :append_session_messages, kwargs ]
      { "ok" => true }
    end

    def execute_session(**kwargs)
      @calls << [ :execute_session, kwargs ]
      { "ok" => true, "execution_id" => "exe_test" }
    end
  end

  TranscriptMessage = Struct.new(:role, :parts_array, :metadata_hash, :created_at, keyword_init: true)
  TranscriptSession = Struct.new(:metadata_hash, :harness_type, keyword_init: true)
  TranscriptEvent = Struct.new(:event_type, :payload_hash, :created_at, keyword_init: true)
  SelectedSession = Struct.new(:thread_key, keyword_init: true)

  setup do
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "threads page renders composer when session database is unavailable" do
    with_recent_first_error do
      get console_threads_url
    end

    assert_response :ok
    assert_select "input[name=q]", count: 0
    assert_select ".console-main-thread-frame aside", count: 0
    assert_select ".console-thread-detail-header .console-page-header"
    assert_select "a[aria-label=?]", "New thread", count: 0
    assert_select "span[aria-label=?]", "New thread disabled", count: 0
    assert_select "textarea[name=prompt][placeholder=?]", "Start a new Centaur thread"
    assert_select "select[name=harness_type] option[value=codex]"
    assert_select "body", text: /No threads yet/
    assert_select "body", text: /Thread database is unavailable/
  end

  test "blank prompt redirects without calling the session api" do
    post console_threads_url, params: { prompt: " " }

    assert_redirected_to console_threads_path
    assert_equal "Enter a message to start a thread.", flash[:alert]
  end

  test "read only mode hides composer controls" do
    with_threads_read_only do
      with_recent_first_error do
        get console_threads_url
      end
    end

    assert_response :ok
    assert_select "textarea[name=prompt]", count: 0
    assert_select "form[action=?]", console_threads_path, count: 0
    assert_select "body", text: /Read-only snapshot/, count: 0
    assert_select "span[aria-label=?]", "New thread disabled", count: 0
    assert_select "a[aria-label=?]", "New thread", count: 0
  end

  test "read only mode blocks posts without calling the session api" do
    client = RecordingClient.new
    with_thread_client(client) do
      with_threads_read_only do
        post console_threads_url, params: { prompt: "Do not run this." }
      end
    end

    assert_redirected_to console_threads_path
    assert_equal "Threads are read-only while browsing a mirrored production snapshot.", flash[:alert]
    assert_empty client.calls
  end

  test "plain threads page redirects to first visible thread" do
    thread_key = "console:auto-select-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)

    get console_threads_url

    assert_redirected_to console_threads_path(thread: thread_key)
  ensure
    delete_console_session(thread_key) if thread_key
  end

  test "direct selected thread appears in sidebar when outside owner filtered list" do
    thread_key = "slack:C0DIRECT:#{SecureRandom.hex(6)}"
    insert_slack_session(
      thread_key,
      slack_user_id: "U_OTHER",
      slack_user_name: "someone-else"
    )

    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select ".console-thread-list a.console-thread-link-active[href=?]",
                  console_threads_path(thread: thread_key),
                  count: 1
  ensure
    delete_console_session(thread_key) if thread_key
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

  test "thread title prefers stored summary metadata when present" do
    controller = Console::ThreadsController.new
    session = TranscriptSession.new(
      metadata_hash: { "summary" => { "title" => "Investigate rollout failure" } },
      harness_type: "codex"
    )

    assert_equal "Investigate rollout failure", controller.send(:thread_title, session)
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

  test "direct selected thread can load outside visible Slack list scope" do
    controller = Console::ThreadsController.new
    direct_thread = SelectedSession.new(thread_key: "slack:C123:1782339173.755169")
    scoped_relation = Object.new
    scoped_relation.define_singleton_method(:where) { |**_kwargs| [] }
    controller.instance_variable_set(:@selected_thread_key, direct_thread.thread_key)
    controller.instance_variable_set(:@starting_new_thread, false)
    controller.instance_variable_set(:@sessions, [])
    controller.define_singleton_method(:direct_selected_session) do |thread_key|
      direct_thread if thread_key == direct_thread.thread_key
    end

    selected = controller.send(:selected_session, scoped_relation, [])

    assert_equal direct_thread, selected
  end

  test "direct selected thread fallback does not bypass console thread scope" do
    controller = Console::ThreadsController.new

    assert_nil controller.send(:direct_selected_session, "console:someone-else")
  end

  test "starting a thread creates appends and executes through the session api" do
    client = RecordingClient.new
    with_thread_client(client) do
      post console_threads_url, params: { prompt: "Reply with PONG.", harness_type: "amp" }
    end

    assert_response :redirect
    assert_match %r{/console/threads\?thread=console%3A}, response.location
    assert_equal [ :create_session, :append_session_messages, :execute_session ],
                 client.calls.map(&:first)

    create_args = client.calls[0].last
    assert_match(/\Aconsole:/, create_args[:thread_key])
    assert_equal "amp", create_args[:harness_type]
    assert_equal "console", create_args[:metadata][:platform]
    assert_equal @operator.email, create_args[:metadata][:actor_email]

    append_args = client.calls[1].last
    assert_equal create_args[:thread_key], append_args[:thread_key]
    message = append_args[:messages].first
    assert_equal "user", message[:role]
    assert_equal [ { type: "text", text: "Reply with PONG." } ], message[:parts]

    execute_args = client.calls[2].last
    assert_equal create_args[:thread_key], execute_args[:thread_key]
    line = JSON.parse(execute_args[:input_lines].first)
    assert_equal "user", line["type"]
    assert_equal create_args[:thread_key], line["thread_key"]
    assert_equal "Reply with PONG.", line.dig("message", "content", 0, "text")
  end

  test "posting to an existing thread appends and executes without creating a session" do
    client = RecordingClient.new
    with_thread_client(client) do
      post console_threads_url,
           params: {
             prompt: "Continue from here.",
             thread_key: "console:existing",
             harness_type: "codex"
           }
    end

    assert_redirected_to console_threads_path(thread: "console:existing")
    assert_equal [ :append_session_messages, :execute_session ],
                 client.calls.map(&:first)

    append_args = client.calls[0].last
    assert_equal "console:existing", append_args[:thread_key]
    assert_equal "reply", append_args[:messages].first[:metadata][:action]

    execute_args = client.calls[1].last
    assert_equal "console:existing", execute_args[:thread_key]
    line = JSON.parse(execute_args[:input_lines].first)
    assert_equal "Continue from here.", line.dig("message", "content", 0, "text")
  end

  private

  def with_thread_client(client)
    original = Console::ThreadsController.client_factory
    Console::ThreadsController.client_factory = -> { client }
    yield
  ensure
    Console::ThreadsController.client_factory = original
  end

  def with_recent_first_error
    singleton = class << CentaurSession; self; end
    original = CentaurSession.method(:recent_first)
    singleton.define_method(:recent_first) { raise ActiveRecord::ConnectionNotEstablished }
    yield
  ensure
    singleton.define_method(:recent_first, original)
  end

  def with_threads_read_only
    original = Console::ThreadsController.read_only_override
    Console::ThreadsController.read_only_override = true
    yield
  ensure
    Console::ThreadsController.read_only_override = original
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

  def delete_console_session(thread_key)
    CentaurSession.connection.execute(
      "delete from sessions where thread_key = #{CentaurSession.connection.quote(thread_key)}"
    )
  end
end
