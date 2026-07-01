require "test_helper"

class Console::ThreadsControllerTest < ActionDispatch::IntegrationTest
  TranscriptMessage = Struct.new(:role, :parts_array, :metadata_hash, :created_at, keyword_init: true)
  TranscriptSession = Struct.new(:metadata_hash, :harness_type, keyword_init: true)
  TranscriptEvent = Struct.new(:event_type, :payload_hash, :created_at, keyword_init: true)
  SelectedSession = Struct.new(:thread_key, keyword_init: true)

  setup do
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "threads page does not render composer when session database is unavailable" do
    with_recent_first_error do
      get console_threads_url
    end

    assert_response :ok
    assert_select "input[name=q]", count: 0
    assert_select ".console-main-thread-frame aside", count: 0
    assert_select ".console-thread-detail-header .console-page-header"
    assert_select "a[aria-label=?]", "New thread", count: 0
    assert_select "span[aria-label=?]", "New thread disabled", count: 0
    assert_select "textarea[name=prompt]", count: 0
    assert_select "select[name=harness_type]", count: 0
    assert_select "form[action=?]", console_threads_path, count: 0
    assert_select "body", text: /No threads yet/
    assert_select "body", text: /Thread database is unavailable/
  end

  test "blank prompt is blocked by read only mode" do
    post console_threads_url, params: { prompt: " " }

    assert_redirected_to console_threads_path
    assert_equal "Threads are read-only while browsing a mirrored production snapshot.", flash[:alert]
  end

  test "threads page hides composer controls" do
    with_recent_first_error do
      get console_threads_url
    end

    assert_response :ok
    assert_select "textarea[name=prompt]", count: 0
    assert_select "form[action=?]", console_threads_path, count: 0
    assert_select "body", text: /Read-only snapshot/, count: 0
    assert_select "span[aria-label=?]", "New thread disabled", count: 0
    assert_select "a[aria-label=?]", "New thread", count: 0
  end

  test "posts are blocked without calling the session api" do
    post console_threads_url, params: { prompt: "Do not run this." }

    assert_redirected_to console_threads_path
    assert_equal "Threads are read-only while browsing a mirrored production snapshot.", flash[:alert]
  end

  test "plain threads page redirects to first visible thread" do
    skip_unless_session_table

    thread_key = "console:auto-select-#{SecureRandom.hex(8)}"
    insert_console_session(thread_key)

    get console_threads_url

    assert_redirected_to console_threads_path(thread: thread_key)
  end

  test "direct selected thread is hidden when the current user did not start it" do
    skip_unless_session_table

    thread_key = "slack:C0DIRECT:#{SecureRandom.hex(6)}"
    insert_slack_session(
      thread_key,
      slack_user_id: "U_OTHER",
      slack_user_name: "someone-else"
    )

    # @operator has no Slack OAuth credential matching U_OTHER, so this thread is
    # outside their owner scope. A direct ?thread= link must not surface it.
    get console_threads_url(thread: thread_key)

    assert_response :ok
    assert_select ".console-thread-list a.console-thread-link-active[href=?]",
                  console_threads_path(thread: thread_key),
                  count: 0
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

  test "visible thread scope ignores Slack credentials without a resolvable team" do
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

    # A credential with no team cannot own threads: no Slack matching is emitted,
    # so the scope falls back to the current user's console threads only.
    refute_includes sql, "thread_key LIKE 'slack:%'"
    refute_includes sql, "uowner"
    assert_includes sql, "thread_key LIKE 'console:%'"
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

  test "starting a thread is blocked without calling the session api" do
    post console_threads_url, params: { prompt: "Reply with PONG.", harness_type: "amp" }

    assert_redirected_to console_threads_path
    assert_equal "Threads are read-only while browsing a mirrored production snapshot.", flash[:alert]
  end

  test "posting to an existing thread is blocked without calling the session api" do
    post console_threads_url,
         params: {
           prompt: "Continue from here.",
           thread_key: "console:existing",
           harness_type: "codex"
         }

    assert_redirected_to console_threads_path(thread: "console:existing")
    assert_equal "Threads are read-only while browsing a mirrored production snapshot.", flash[:alert]
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
    assert_select ".console-thread-empty", text: /No recent threads/
  end

  # Fix 5: selected_messages must return the NEWEST MESSAGE_LIMIT messages, in
  # oldest-first display order. A previous ascending order + limit returned the
  # oldest N and dropped the newest for long threads.
  test "selected_messages query fetches newest messages first with a limit" do
    controller = Console::ThreadsController.new
    controller.instance_variable_set(
      :@selected_session,
      SelectedSession.new(thread_key: "console:ordering")
    )

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

  private

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
end
