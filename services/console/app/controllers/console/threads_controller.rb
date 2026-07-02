class Console::ThreadsController < ApplicationController
  layout "console"

  THREAD_LIMIT = 250
  MESSAGE_LIMIT = 80
  EXECUTION_LIMIT = 8
  TRANSCRIPT_EVENT_LIMIT = 80
  PANEL_LIMIT = 4
  THINKING_EVENT_LIMIT = 200
  # Messages and thinking precede the terminal event for a same-timestamp tie.
  TRANSCRIPT_SOURCE_ORDER = { message: 0, thinking: 1, event: 2 }.freeze
  SLACK_PROVIDER = Oauth::Providers::Slack::KEY
  SLACK_THREAD_OWNER_METADATA_KEYS = %w[slack_user_id actor_user_id user_id].freeze
  SLACK_THREAD_TEAM_METADATA_KEYS = %w[slack_team_id team_id home_team_id].freeze
  SLACK_CREDENTIAL_USER_LABEL_KEYS = %w[slack_user_id].freeze
  SLACK_CREDENTIAL_EMAIL_LABEL_KEYS = %w[email slack_email].freeze
  SLACK_TEAM_LABEL = "slack_team_id"
  CONSOLE_THREAD_OWNER_METADATA_KEYS = %w[actor_email user_email].freeze
  SLACK_USER_ID_PATTERN = /\A[UW][A-Z0-9]+\z/.freeze
  SLACK_MENTION_PATTERN = /<@([UW][A-Z0-9]+)(?:\|([^>]+))?>|@([UW][A-Z0-9]+)/.freeze
  READ_ONLY_REASON =
    "Chats are read-only while browsing a mirrored production snapshot.".freeze
  # Deploy-time default-model overrides: the same env vars deployers set in
  # sandbox.extraEnv to change the harness model, mirrored onto the Console by
  # the chart. Amp has no fixed default model, so it is intentionally absent.
  HARNESS_DEFAULT_MODEL_ENVS = {
    "claudecode" => "CLAUDE_MODEL",
    "codex" => "CODEX_MODEL"
  }.freeze
  # Harness config files carrying each harness's baked-in default model, used
  # when no env override is set. Resolved against CENTAUR_HARNESS_CONFIG_DIR
  # (the sandbox entrypoint's variable) or the repo checkout's harness/
  # directory; absent files (e.g. in the production image, whose build context
  # is services/console) simply yield no default.
  HARNESS_CONFIG_FILES = {
    "claudecode" => "claude/settings.json",
    "codex" => "codex/config.toml"
  }.freeze

  SlackThreadOwner = Struct.new(:user_id, :team_id, keyword_init: true)

  helper_method :thread_title,
                :thread_source_icon,
                :thread_source_label,
                :thread_harness_label,
                :thread_model_label,
                :thread_user_label,
                :thread_message_text,
                :thread_text_preview,
                :thread_status_classes

  def index
    @query = params[:q].to_s.strip
    requested_keys = requested_thread_keys
    @selected_thread_key = requested_keys.first.to_s
    @pane_thread_keys = requested_keys.drop(1)
    @starting_new_thread = params[:new].present?
    @thread_db_unavailable = false
    @thread_not_found = false

    load_threads
    if @thread_not_found
      render status: :not_found
      return
    end
    redirect_to_first_thread if auto_select_first_thread?
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_threads_load_failed error=#{e.class}: #{e.message}")
    empty_thread_state
    @thread_db_unavailable = true
  end

  def create
    redirect_to(
      console_threads_path(thread: params[:thread_key].presence),
      alert: READ_ONLY_REASON
    )
  end

  # Lazily-loaded sidebar thread list, requested by the Turbo Frame in
  # layouts/console.html.erb. Runs the cross-database sessions query out of band
  # so it never blocks the primary page render. Renders only the frame partial
  # (no layout). DB errors leave the list empty via load_console_sidebar_threads.
  def sidebar
    load_console_sidebar_threads
    render partial: "console/threads/sidebar_threads", layout: false
  end

  private

  def load_threads
    session_scope = visible_thread_scope
    base_sessions = session_scope.recent_first.limit(THREAD_LIMIT).to_a
    keys = base_sessions.map(&:thread_key).uniq

    @latest_messages = latest_messages_for(keys)
    @latest_executions = latest_executions_for(keys)
    @message_counts = count_records(CentaurSessionMessage, keys)
    @execution_counts = count_records(CentaurSessionExecution, keys)

    @sessions = base_sessions.select { |session| matches_query?(session) }
    @selected_session = selected_session(session_scope, base_sessions)
    if @thread_not_found
      @pane_sessions = []
      @thread_panels = []
      @selected_messages = []
      @selected_executions = []
      @selected_events = []
      @selected_transcript_items = []
      return
    end
    @pane_sessions = resolve_pane_sessions(session_scope, base_sessions)
    load_selected_session_summaries(keys)
    @selected_thread_key = @selected_session&.thread_key.to_s
    @thread_panels = build_thread_panels
    @selected_transcript_items = @thread_panels.first&.dig(:transcript_items) || []
  end

  def empty_thread_state
    @thread_not_found = false
    @sessions = []
    @selected_session = nil
    @pane_sessions = []
    @thread_panels = []
    @selected_messages = []
    @selected_executions = []
    @selected_events = []
    @selected_transcript_items = []
    @latest_messages = {}
    @latest_executions = {}
    @message_counts = {}
    @execution_counts = {}
  end

  def matches_query?(session)
    return true if @query.blank?

    needle = @query.downcase
    [
      session.thread_key,
      thread_title(session),
      thread_source_label(session),
      thread_user_label(session),
      thread_text_preview(@latest_messages[session.thread_key])
    ].any? { |value| value.to_s.downcase.include?(needle) }
  end

  def selected_session(session_scope, base_sessions)
    return nil if @starting_new_thread

    if @selected_thread_key.present?
      selected = base_sessions.find { |session| session.thread_key == @selected_thread_key }
      # Resolve the key through the owner scope so a directly linked chat only
      # loads when the current user started it. base_sessions is capped at
      # THREAD_LIMIT, so this also recovers an owned thread beyond that window.
      selected ||= session_scope.where(thread_key: @selected_thread_key).first
      # A directly requested key outside the owner scope renders as 404 rather
      # than silently falling back to another chat, so nonexistent and
      # inaccessible chats are indistinguishable to the viewer.
      @thread_not_found = selected.nil?
      return selected
    end
    @sessions.first
  end

  def auto_select_first_thread?
    params[:thread].blank? && !@starting_new_thread && @query.blank? && @selected_session.present?
  end

  # The thread param carries up to PANEL_LIMIT comma-separated thread keys; the
  # first is the primary thread and the rest are extra split-view panes
  # (Cmd/Ctrl-click on a sidebar thread appends its key).
  def requested_thread_keys
    params[:thread].to_s.split(",").map(&:strip).reject(&:blank?).uniq.first(PANEL_LIMIT)
  end

  # Extra split-view panes resolve through the same owner scope as the primary
  # thread, so a crafted ?thread= list cannot surface another user's thread.
  # Unowned keys are dropped silently.
  def resolve_pane_sessions(session_scope, base_sessions)
    keys = @pane_thread_keys - [ @selected_session&.thread_key ]
    keys.filter_map do |key|
      base_sessions.find { |session| session.thread_key == key } ||
        session_scope.where(thread_key: key).first
    end
  end

  def build_thread_panels
    sessions = ([ @selected_session ] + Array(@pane_sessions)).compact
      .uniq(&:thread_key)
      .first(PANEL_LIMIT)
    return [] if sessions.empty?

    # Build the primary panel last so the @selected_* thread state (used by the
    # page header and mention-resolution memos) ends on the primary thread.
    extra_panels = sessions.drop(1).map { |session| thread_panel_for(session) }
    [ thread_panel_for(sessions.first) ] + extra_panels
  end

  def thread_panel_for(session)
    @selected_session = session
    @selected_messages = selected_messages
    @selected_executions = selected_executions
    @selected_events = selected_events
    reset_selected_thread_memos

    {
      session: session,
      thread_key: session.thread_key,
      transcript_items: selected_transcript_items
    }
  end

  # Mention labels and inferred bot ids are memoized off the selected thread's
  # messages and events, so they must be recomputed per panel.
  def reset_selected_thread_memos
    @slack_mention_labels_by_id = nil
    @slack_bot_user_ids = nil
  end

  def redirect_to_first_thread
    redirect_to console_threads_path(thread: @selected_session.thread_key)
  end

  def load_selected_session_summaries(loaded_keys)
    missing_keys = ([ @selected_session ] + Array(@pane_sessions)).compact
      .map(&:thread_key)
      .uniq
      .reject { |key| loaded_keys.include?(key) }
    return if missing_keys.empty?

    @latest_messages.merge!(latest_messages_for(missing_keys))
    @latest_executions.merge!(latest_executions_for(missing_keys))
    @message_counts.merge!(count_records(CentaurSessionMessage, missing_keys))
    @execution_counts.merge!(count_records(CentaurSessionExecution, missing_keys))
  end

  def visible_thread_scope
    slack_owners = slack_thread_owners_for_current_user
    conditions = [
      console_thread_owner_sql,
      (slack_thread_owner_sql(slack_owners) if slack_owners.any?)
    ].compact

    return CentaurSession.where("1=0") if conditions.empty?

    CentaurSession.where(conditions.map { |condition| "(#{condition})" }.join(" OR "))
  end

  def console_thread_owner_sql
    email = normalize_email(current_user&.email)
    return if email.blank?

    console_source = [
      "thread_key LIKE 'console:%'",
      "metadata ->> 'platform' = 'console'",
      "metadata ->> 'source' = 'console'"
    ].join(" OR ")
    owner_clauses = CONSOLE_THREAD_OWNER_METADATA_KEYS.map do |key|
      "lower(metadata ->> #{sql_quote(key)}) = #{sql_quote(email)}"
    end

    "(#{console_source}) AND (#{owner_clauses.join(" OR ")})"
  end

  def slack_thread_owners_for_current_user
    @slack_thread_owners_for_current_user ||= begin
      if current_user
        subjects = slack_identity_subjects_for_current_user
        emails = slack_identity_emails_for_current_user

        if subjects.empty? && emails.empty?
          []
        else
          credentials = BrokerCredential
            .joins(:oauth_app)
            .includes(:oauth_app)
            .where(oauth_apps: { provider: SLACK_PROVIDER })
            .where(slack_oauth_credential_owner_sql(subjects: subjects, emails: emails))

          credential_owners = credentials.filter_map do |credential|
            user_id = first_present(
              credential.provider_subject,
              *SLACK_CREDENTIAL_USER_LABEL_KEYS.map { |key| credential.labels&.[](key) }
            )
            next if user_id.blank?

            SlackThreadOwner.new(
              user_id: user_id,
              team_id: first_present(
                credential.labels&.[](SLACK_TEAM_LABEL),
                credential.oauth_app&.labels&.[](SLACK_TEAM_LABEL)
              )
            )
          end

          # A Slack OIDC sign-in stores the workspace user id (U…) as the
          # identity subject — the same id slackbotv2 writes into session
          # metadata — so SSO alone owns those threads even when the user has
          # not minted a broker credential through the connect flow.
          identity_owners = subjects.map do |subject|
            SlackThreadOwner.new(user_id: subject, team_id: nil)
          end

          (credential_owners + identity_owners)
            .uniq { |owner| [ normalize_key(owner.user_id), normalize_key(owner.team_id) ] }
        end
      else
        []
      end
    end
  end

  def slack_identity_subjects_for_current_user
    current_user.user_identities
      .where(provider: SLACK_PROVIDER)
      .pluck(:subject)
      .filter_map { |value| normalize_key(value) }
      .uniq
  end

  def slack_identity_emails_for_current_user
    ([ current_user.email ] + current_user.user_identities.where(provider: SLACK_PROVIDER).pluck(:email))
      .filter_map { |value| normalize_email(value) }
      .uniq
  end

  def slack_oauth_credential_owner_sql(subjects:, emails:)
    clauses = []
    if subjects.any?
      subject_values = sql_list(subjects)
      clauses << "lower(broker_credentials.provider_subject) IN (#{subject_values})"
      SLACK_CREDENTIAL_USER_LABEL_KEYS.each do |key|
        clauses << "lower(broker_credentials.labels ->> #{sql_quote(key)}) IN (#{subject_values})"
      end
    end

    if emails.any?
      email_values = sql_list(emails)
      clauses << "lower(broker_credentials.provider_email) IN (#{email_values})"
      SLACK_CREDENTIAL_EMAIL_LABEL_KEYS.each do |key|
        clauses << "lower(broker_credentials.labels ->> #{sql_quote(key)}) IN (#{email_values})"
      end
    end

    clauses.join(" OR ")
  end

  def slack_thread_owner_sql(owners)
    slack_source = [
      "thread_key LIKE 'slack:%'",
      "metadata ->> 'platform' = 'slack'",
      "metadata ->> 'source' = 'slackbotv2'"
    ].join(" OR ")

    owner_clauses = owners.map do |owner|
      user_id = normalize_key(owner.user_id)
      user_clauses = SLACK_THREAD_OWNER_METADATA_KEYS.map do |key|
        "lower(metadata ->> #{sql_quote(key)}) = #{sql_quote(user_id)}"
      end
      owner_clause = "(#{user_clauses.join(" OR ")})"

      # Team scoping narrows the match only when the owning credential exposes a
      # team. slackbotv2 uses slack:CHANNEL:TS thread keys and does not record a
      # slack_team_id, so requiring a team would hide otherwise-owned threads.
      if owner.team_id.present?
        team_id = normalize_key(owner.team_id)
        team_clauses = SLACK_THREAD_TEAM_METADATA_KEYS.map do |key|
          "lower(metadata ->> #{sql_quote(key)}) = #{sql_quote(team_id)}"
        end
        team_clauses << "lower(split_part(thread_key, ':', 2)) = #{sql_quote(team_id)}"
        owner_clause = "(#{owner_clause} AND (#{team_clauses.join(" OR ")}))"
      end

      owner_clause
    end

    "(#{slack_source}) AND (#{owner_clauses.join(" OR ")})"
  end

  def first_present(*values)
    values.find(&:present?)
  end

  def normalize_key(value)
    value.to_s.strip.downcase.presence
  end

  def normalize_email(value)
    value.to_s.strip.downcase.presence
  end

  def sql_list(values)
    values.map { |value| sql_quote(value) }.join(", ")
  end

  def sql_quote(value)
    ActiveRecord::Base.connection.quote(value.to_s)
  end

  def selected_messages
    return [] unless @selected_session

    # Fetch the newest MESSAGE_LIMIT messages, then reverse for oldest-first
    # display. Ordering ascending before LIMIT would return the OLDEST N and
    # drop the newest for long threads (mirrors selected_events below).
    CentaurSessionMessage
      .where(thread_key: @selected_session.thread_key)
      .order(created_at: :desc, message_id: :desc)
      .limit(MESSAGE_LIMIT)
      .to_a
      .reverse
  end

  def selected_executions
    return [] unless @selected_session

    CentaurSessionExecution
      .where(thread_key: @selected_session.thread_key)
      .order(created_at: :desc, execution_id: :desc)
      .limit(EXECUTION_LIMIT)
      .to_a
  end

  def selected_events
    return [] unless @selected_session

    CentaurSessionEvent
      .where(thread_key: @selected_session.thread_key)
      .where(event_type: %w[
        session.execution_completed
        session.execution_failed
        session.execution_cancelled
      ])
      .order(event_id: :desc)
      .limit(TRANSCRIPT_EVENT_LIMIT)
      .to_a
      .reverse
  end

  def selected_transcript_items
    message_items = @selected_messages.map { |message| transcript_item_for_message(message) }

    event_items = @selected_events.filter_map { |event| transcript_item_for_event(event) }

    thinking_items = selected_thinking_items

    (message_items + thinking_items + event_items).sort_by do |item|
      [ item[:created_at] || Time.zone.at(0), TRANSCRIPT_SOURCE_ORDER[item[:source]] || 0 ]
    end
  end

  # The api-rs stdout pump persists every harness output line verbatim as a
  # session.output.line event whose payload is a JSON-encoded string. Codex
  # reasoning arrives as item/completed notifications with item.type ==
  # "reasoning" carrying the full accumulated thinking text; Claude Code
  # stream-json persists each assistant API message whose content can include
  # "thinking" blocks. The SQL LIKE filter keeps the query from paging through
  # the whole firehose; exact matching happens here.
  def selected_thinking_items
    return [] unless @selected_session

    CentaurSessionEvent
      .where(thread_key: @selected_session.thread_key)
      .where(event_type: "session.output.line")
      .where("payload::text LIKE '%reasoning%' OR payload::text LIKE '%thinking%'")
      .order(event_id: :desc)
      .limit(THINKING_EVENT_LIMIT)
      .to_a
      .reverse
      .filter_map { |event| thinking_transcript_item(event) }
  end

  def thinking_transcript_item(event)
    line = event.payload
    return nil unless line.is_a?(String)

    value = JSON.parse(line)
    return nil unless value.is_a?(Hash)

    text = reasoning_event_text(value) || claude_thinking_text(value)
    return nil if text.blank?

    {
      role: "thinking",
      label: "Thinking",
      align: :start,
      text: text,
      created_at: event.created_at,
      source: :thinking
    }
  rescue JSON::ParserError
    nil
  end

  def reasoning_event_text(value)
    method = (value["method"] || value["type"]).to_s.tr("/", ".")
    return nil unless method == "item.completed"

    item = value.dig("params", "item") || value["item"]
    return nil unless item.is_a?(Hash) && item["type"].to_s == "reasoning"

    reasoning_item_text(item)
  end

  # Claude Code's stream-json output persists each assistant API message as
  # {"type":"assistant","message":{"content":[...]}} where extended thinking
  # arrives in content blocks of type "thinking" (text under the "thinking"
  # key). Partial stream_event lines never carry type == "assistant", so each
  # thinking block surfaces exactly once.
  def claude_thinking_text(value)
    return nil unless value["type"].to_s == "assistant"

    message = value["message"]
    content = message.is_a?(Hash) ? message["content"] : value["content"]
    return nil unless content.is_a?(Array)

    content.filter_map do |part|
      next unless part.is_a?(Hash) && part["type"].to_s == "thinking"

      part["thinking"].presence || part["text"].presence
    end.join("\n").strip.presence
  end

  # Claude/Amp reasoning lands in content (full text); Codex-native reasoning
  # may only carry a summary. Prefer the fullest field available.
  def reasoning_item_text(item)
    [
      item["text"],
      reasoning_part_text(item["content"]),
      reasoning_part_text(item["summary"])
    ].find(&:present?)
  end

  def reasoning_part_text(value)
    entries = value.is_a?(Array) ? value : [ value ]
    entries.filter_map do |part|
      case part
      when String then part
      when Hash then part["text"].to_s
      end
    end.join("\n").strip.presence
  end

  def latest_messages_for(keys)
    return {} if keys.empty?

    CentaurSessionMessage
      .where(thread_key: keys)
      .select("distinct on (thread_key) session_messages.*")
      .order(Arel.sql("thread_key, created_at desc, message_id desc"))
      .index_by(&:thread_key)
  end

  def latest_executions_for(keys)
    return {} if keys.empty?

    CentaurSessionExecution
      .where(thread_key: keys)
      .select("distinct on (thread_key) session_executions.*")
      .order(Arel.sql("thread_key, created_at desc, execution_id desc"))
      .index_by(&:thread_key)
  end

  def transcript_item_for_message(message)
    metadata = message_metadata_hash(message)

    {
      role: message.role,
      label: transcript_message_label(message.role, metadata),
      align: transcript_message_align(message.role, metadata),
      text: resolve_slack_mentions(thread_message_text(message)),
      created_at: message.created_at,
      source: :message
    }
  end

  def count_records(model, keys)
    return {} if keys.empty?

    model.where(thread_key: keys).group(:thread_key).count
  end

  def thread_title(session)
    stored = stored_session_title(session)
    return clip_one_line(stored, 80) if stored

    metadata = session.metadata_hash
    summary = metadata["summary"]
    title = metadata["title"].presence ||
      metadata["generated_title"].presence ||
      metadata["summary_title"].presence ||
      metadata["thread_title"].presence ||
      (metadata["thread"].is_a?(Hash) ? metadata["thread"]["title"] : nil).presence ||
      (metadata["summary"].is_a?(Hash) ? metadata["summary"]["title"] : nil).presence ||
      (summary if summary.is_a?(String)).presence ||
      metadata["subject"].presence ||
      metadata["issue_title"].presence
    return generated_thread_title(title) if title

    preview = thread_text_preview(@latest_messages[session.thread_key])
    generated = generated_thread_title(preview)
    return generated if generated.present?

    human_thread_key(session.thread_key)
  end

  # The title api-rs generates and writes onto sessions.title after a message
  # append. Guarded because sessions mirrored from a snapshot taken before the
  # title migration have no such column.
  def stored_session_title(session)
    session.title.presence if session.respond_to?(:title)
  end

  def thread_source_icon(session)
    thread_source_key(session) == "slack" ? "slack" : "computer"
  end

  def thread_source_label(session)
    source_label(thread_source_key(session))
  end

  def thread_harness_label(session)
    case session.harness_type.to_s
    when "codex" then "Codex"
    when "claudecode" then "Claude Code"
    when "amp" then "Amp"
    else source_label(session.harness_type)
    end
  end

  # Model the thread most recently ran on. slackbotv2 records the effective
  # model in execution metadata; for older rows without it, fall back to the
  # deployment's default the way the sandbox resolves it: CLAUDE_MODEL /
  # CODEX_MODEL env override first, then the model pinned in the harness
  # config files when they are present. Nil (segment omitted) when none of
  # those sources know the model.
  def thread_model_label(session)
    model = recorded_model(@latest_executions&.[](session.thread_key)&.metadata) ||
      recorded_model(session.metadata_hash) ||
      default_model_for_harness(session.harness_type.to_s)
    # Uppercased for display, matching the Slack Console-link context line.
    model&.upcase
  end

  def recorded_model(metadata)
    return unless metadata.is_a?(Hash)

    metadata["model"].presence
  end

  def default_model_for_harness(harness_type)
    env_name = HARNESS_DEFAULT_MODEL_ENVS[harness_type]
    return unless env_name

    ENV[env_name].presence || self.class.baked_harness_default_model(harness_type)
  end

  # Cached per (config dir, harness): the files are immutable within a deploy,
  # and the dir key keeps tests with CENTAUR_HARNESS_CONFIG_DIR overrides
  # isolated.
  def self.baked_harness_default_model(harness_type)
    relative = HARNESS_CONFIG_FILES[harness_type]
    return unless relative

    dir = ENV["CENTAUR_HARNESS_CONFIG_DIR"].presence ||
      Rails.root.join("..", "..", "harness").to_s
    cache = (@baked_harness_default_models ||= {})
    key = [ dir, harness_type ]
    return cache[key] if cache.key?(key)

    cache[key] = parse_harness_default_model(File.join(dir, relative))
  end

  def self.parse_harness_default_model(path)
    return unless File.file?(path)

    contents = File.read(path)
    model =
      if path.end_with?(".json")
        parsed = JSON.parse(contents)
        parsed["model"] if parsed.is_a?(Hash)
      else
        # Minimal TOML: the top-level `model = "..."` line in codex/config.toml.
        contents[/^model\s*=\s*"([^"]+)"/, 1]
      end
    model.presence
  rescue JSON::ParserError, SystemCallError
    nil
  end

  def thread_source_key(session)
    metadata = session.metadata_hash
    (
      metadata["repository"].presence ||
      metadata["repo"].presence ||
      metadata["platform"].presence ||
      metadata["source"].presence ||
      session.thread_key.to_s.split(":").first.presence ||
      "unknown"
    ).to_s.downcase
  end

  def source_label(value)
    normalized = value.to_s.tr("_-", " ").squish
    return "Slack" if normalized.casecmp("slack").zero?
    return "Console" if normalized.casecmp("console").zero?
    return "Unknown" if normalized.blank?

    normalized.split.map(&:capitalize).join(" ")
  end

  def thread_user_label(session)
    metadata = session.metadata_hash
    metadata["user_name"].presence ||
      metadata["user_email"].presence ||
      metadata["actor_email"].presence ||
      metadata["slack_user_name"].presence ||
      metadata["actor_user_id"].presence ||
      metadata["user_id"].presence ||
      "unknown"
  end

  def thread_message_text(message)
    return "" unless message

    message.parts_array.filter_map do |part|
      next unless part.is_a?(Hash)

      case part["type"]
      when "text" then part["text"].to_s
      when "image" then "[image]"
      when "document" then "[document]"
      end
    end.join("\n").squish
  end

  def thread_text_preview(message)
    thread_message_text(message).truncate(120)
  end

  def generated_thread_title(text)
    title = text.to_s
      .gsub(/<@[A-Z0-9]+(?:\|[^>]+)?>/, "")
      .sub(/\A\s*@?centaur\b[:,]?\s*/i, "")
      .sub(/\A\s*@?U[A-Z0-9]+\b[:,]?\s*/i, "")
      .sub(/\A\s*@\S+\s+/, "")
      .strip
    title = title.sub(/\A[*_]{1,2}(.+?)[*_]{1,2}\s*/, "\\1 ").squish
    clip_one_line(title, 80)
  end

  def clip_one_line(value, max)
    one_line = value.to_s.gsub(/\s+/, " ").strip
    return one_line if one_line.length <= max

    "#{one_line.slice(0, [ max - 3, 0 ].max).rstrip}..."
  end

  def transcript_item_for_event(event)
    case event.event_type
    when "session.execution_completed"
      text = resolve_slack_mentions(
        terminal_payload_text(event.payload_hash["result_text"] || event.payload_hash)
      )
      role = "assistant"
      label = assistant_author_label
    when "session.execution_failed"
      text = terminal_payload_text(event.payload_hash["error"] || event.payload_hash)
      role = "system"
      label = role
    when "session.execution_cancelled"
      text = "Execution cancelled."
      role = "system"
      label = role
    end

    return nil if text.blank?

    {
      role: role,
      label: label,
      align: :start,
      text: text,
      created_at: event.created_at,
      source: :event
    }
  end

  def transcript_message_align(role, metadata)
    return :end if slack_message_from_current_user?(metadata)
    return :start if slack_message?(metadata)

    role == "user" ? :end : :start
  end

  def transcript_message_label(role, metadata)
    return slack_message_author_label(metadata) if slack_message?(metadata)
    return assistant_author_label if role == "assistant"

    role
  end

  def slack_message?(metadata)
    metadata["platform"] == "slack" || metadata["source"] == "slackbotv2"
  end

  def slack_message_from_current_user?(metadata)
    slack_user_id = normalize_key(metadata["slack_user_id"] || metadata["user_id"])

    slack_user_id.present? && current_slack_user_ids.include?(slack_user_id)
  end

  def current_slack_user_ids
    @current_slack_user_ids ||= slack_thread_owners_for_current_user
      .filter_map { |owner| normalize_key(owner.user_id) }
      .uniq
  end

  def slack_message_author_label(metadata)
    return assistant_author_label if slack_bot_user_id?(metadata["slack_user_id"])

    current_user_metadata =
      slack_message_from_current_user?(metadata) ? @selected_session&.metadata_hash : nil

    label_from_metadata(current_user_metadata) ||
      slack_resolved_user_label(metadata) ||
      label_from_metadata(metadata) ||
      "slack"
  end

  def slack_resolved_user_label(metadata)
    slack_user_id = normalize_key(metadata["slack_user_id"] || metadata["user_id"])
    return if slack_user_id.blank?

    slack_mention_labels_by_id[slack_user_id]
  end

  def label_from_metadata(metadata)
    return nil unless metadata

    [
      metadata["slack_display_name"],
      metadata["slack_user_name"],
      metadata["user_name"],
      metadata["actor_user_id"],
      metadata["user_id"],
      metadata["slack_user_id"]
    ].find(&:present?)
  end

  def resolve_slack_mentions(text)
    text.to_s.gsub(SLACK_MENTION_PATTERN) do
      user_id = Regexp.last_match(1).presence || Regexp.last_match(3)
      explicit_label = Regexp.last_match(2)
      mention_label = slack_mention_labels_by_id[normalize_key(user_id)] ||
        format_slack_mention_label(explicit_label) ||
        "@#{user_id}"

      mention_label
    end
  end

  def slack_mention_labels_by_id
    @slack_mention_labels_by_id ||= begin
      user_ids = slack_user_ids_from_selected_thread
      database_labels = slack_user_display_labels_from_database(user_ids)
      session_metadata_labels = slack_user_display_labels_from_session_messages(user_ids)
      metadata_labels = slack_user_display_labels_from_metadata
      bot_labels = slack_bot_user_ids.index_with { assistant_author_label }

      metadata_labels.merge(session_metadata_labels).merge(database_labels).merge(bot_labels)
    end
  end

  def slack_user_ids_from_selected_thread
    ids = []
    ids.concat(slack_user_ids_from_metadata(@selected_session&.metadata_hash))

    Array(@selected_messages).each do |message|
      ids.concat(slack_user_ids_from_metadata(message_metadata_hash(message)))
      ids.concat(slack_mention_user_ids(thread_message_text(message)))
    end

    Array(@selected_events).each do |event|
      ids.concat(slack_mention_user_ids(terminal_payload_text(event.payload_hash)))
    end

    ids.filter_map { |value| normalize_key(value) }.uniq
  end

  def slack_user_display_labels_from_metadata
    labels = {}
    metadata_sources = [ @selected_session&.metadata_hash ]
    metadata_sources.concat(Array(@selected_messages).map { |message| message_metadata_hash(message) })

    metadata_sources.each do |metadata|
      user_id = normalize_key(metadata&.[]("slack_user_id") || metadata&.[]("user_id"))
      next if user_id.blank?

      label = slack_mention_label_from_metadata(metadata)
      labels[user_id] = label if label.present?
    end

    labels
  end

  def slack_user_display_labels_from_database(user_ids)
    user_ids = user_ids.filter_map { |value| normalize_key(value) }.uniq
    return {} if user_ids.empty?

    connection = CentaurSessionRecord.connection
    return {} unless connection.data_source_exists?("slack_sync_users")

    SlackSyncUser
      .where("lower(user_id) IN (?)", user_ids)
      .pluck(:user_id, :user_name, :display_name, :real_name)
      .each_with_object({}) do |(user_id, user_name, display_name, real_name), labels|
        user_id = normalize_key(user_id)
        label = slack_mention_label_from_values(user_name, display_name, real_name)
        labels[user_id] = label if user_id.present? && label.present?
      end
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.debug("console_threads_slack_user_lookup_failed error=#{e.class}: #{e.message}")
    {}
  end

  def slack_user_display_labels_from_session_messages(user_ids)
    user_ids = user_ids.filter_map { |value| normalize_key(value) }.uniq
    return {} if user_ids.empty?

    rows = CentaurSessionMessage
      .where(<<~SQL.squish, user_ids)
        lower(coalesce(
          nullif(metadata ->> 'slack_user_id', ''),
          nullif(metadata ->> 'user_id', ''),
          nullif(metadata ->> 'actor_user_id', '')
        )) IN (?)
      SQL
      .order(created_at: :desc, message_id: :desc)
      .pluck(
        Arel.sql("metadata ->> 'slack_user_id'"),
        Arel.sql("metadata ->> 'user_id'"),
        Arel.sql("metadata ->> 'actor_user_id'"),
        Arel.sql("metadata ->> 'slack_user_name'"),
        Arel.sql("metadata ->> 'user_name'"),
        Arel.sql("metadata ->> 'slack_display_name'"),
        Arel.sql("metadata ->> 'display_name'")
      )

    rows.each_with_object({}) do |row, labels|
      slack_user_id, user_id_value, actor_user_id, slack_user_name, user_name, slack_display_name, display_name = row
      user_id = normalize_key(slack_user_id || user_id_value || actor_user_id)
      next if user_id.blank? || labels.key?(user_id)

      label = slack_mention_label_from_values(
        slack_user_name,
        user_name,
        slack_display_name,
        display_name
      )
      labels[user_id] = label if label.present?
    end
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.debug("console_threads_slack_message_metadata_lookup_failed error=#{e.class}: #{e.message}")
    {}
  end

  def slack_mention_label_from_metadata(metadata)
    return nil unless metadata

    slack_mention_label_from_values(
      metadata["slack_user_name"],
      metadata["user_name"],
      metadata["slack_display_name"],
      metadata["display_name"]
    )
  end

  def slack_mention_label_from_values(*values)
    values
      .map { |value| value.to_s.strip }
      .reject(&:blank?)
      .reject { |value| slack_user_id?(value) }
      .map { |value| format_slack_mention_label(value) }
      .find(&:present?)
  end

  def format_slack_mention_label(value)
    value = value.to_s.strip
    return nil if value.blank?

    "@#{value.delete_prefix("@")}"
  end

  def slack_mention_user_ids(text)
    text.to_s.scan(SLACK_MENTION_PATTERN).filter_map do |native_id, _label, plain_id|
      native_id.presence || plain_id
    end
  end

  def slack_user_ids_from_metadata(metadata)
    return [] unless metadata

    %w[slack_user_id user_id actor_user_id].filter_map { |key| metadata[key].presence }
  end

  def slack_bot_user_id?(user_id)
    slack_bot_user_ids.include?(normalize_key(user_id))
  end

  def slack_bot_user_ids
    @slack_bot_user_ids ||= begin
      ids = [
        ConsoleEnv["SLACK_BOT_USER_ID"],
        ENV["SLACK_BOT_USER_ID"]
      ]

      ids.concat(inferred_slack_bot_user_ids)
      ids.filter_map { |value| normalize_key(value) }.uniq
    end
  end

  def inferred_slack_bot_user_ids
    ids = []

    Array(@selected_messages).each do |message|
      metadata = message_metadata_hash(message)
      if ActiveModel::Type::Boolean.new.cast(metadata["is_mention"])
        ids << slack_mention_user_ids(thread_message_text(message)).first
      end
    end

    terminal_texts = Array(@selected_events).filter_map do |event|
      next unless event.event_type == "session.execution_completed"

      terminal_payload_text(event.payload_hash["result_text"] || event.payload_hash).presence
    end

    if terminal_texts.any?
      Array(@selected_messages).each do |message|
        text = thread_message_text(message)
        next unless terminal_texts.include?(text)

        ids.concat(slack_user_ids_from_metadata(message_metadata_hash(message)))
      end
    end

    ids.compact
  end

  def slack_user_id?(value)
    value.to_s.strip.match?(SLACK_USER_ID_PATTERN)
  end

  def assistant_author_label
    format_slack_mention_label(
      ConsoleEnv["SLACKBOTV2_USER_NAME"].presence ||
        ENV["SLACKBOTV2_USER_NAME"].presence ||
        "ai"
    )
  end

  def message_metadata_hash(message)
    return message.metadata_hash if message.respond_to?(:metadata_hash)

    metadata = message.respond_to?(:metadata) ? message.metadata : nil
    metadata.is_a?(Hash) ? metadata : {}
  end

  def terminal_payload_text(value)
    case value
    when String
      value.strip
    when Array
      value.lazy.map { |entry| terminal_payload_text(entry) }.find(&:present?).to_s
    when Hash
      %w[result result_text text final_text message delta content params].each do |key|
        text = terminal_payload_text(value[key])
        return text if text.present?
      end
      ""
    else
      ""
    end
  end

  def thread_status_classes(status)
    case status.to_s
    when "active", "running", "queued"
      "bg-centaur-500/10 text-centaur-300 ring-centaur-500/25"
    when "failed", "error"
      "bg-red-500/10 text-red-300 ring-red-500/25"
    when "completed"
      "bg-zinc-500/10 text-zinc-300 ring-zinc-500/25"
    else
      "bg-amber-500/10 text-amber-300 ring-amber-500/25"
    end
  end

  def human_thread_key(thread_key)
    source, *parts = thread_key.to_s.split(":")
    return thread_key if parts.empty?

    "#{source.titleize}: #{parts.last}"
  end
end
