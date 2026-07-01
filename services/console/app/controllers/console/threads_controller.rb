require "securerandom"

class Console::ThreadsController < ApplicationController
  layout "console"

  class_attribute :client_factory, default: -> { CentaurApiClient.new }
  class_attribute :read_only_override, default: nil

  SUPPORTED_HARNESSES = %w[codex amp claudecode].freeze
  THREAD_LIMIT = 250
  MESSAGE_LIMIT = 80
  EXECUTION_LIMIT = 8
  TRANSCRIPT_EVENT_LIMIT = 80
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
    "Threads are read-only while browsing a mirrored production snapshot.".freeze

  SlackThreadOwner = Struct.new(:user_id, :team_id, keyword_init: true)

  helper_method :thread_title,
                :thread_source_icon,
                :thread_source_label,
                :thread_harness_label,
                :thread_user_label,
                :thread_message_text,
                :thread_text_preview,
                :thread_status_classes

  def index
    @query = params[:q].to_s.strip
    @selected_thread_key = params[:thread].to_s
    @starting_new_thread = params[:new].present?
    @thread_db_unavailable = false
    @threads_read_only = threads_read_only?
    @threads_read_only_reason = threads_read_only_reason

    load_threads
    redirect_to_first_thread if auto_select_first_thread?
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_threads_load_failed error=#{e.class}: #{e.message}")
    empty_thread_state
    @thread_db_unavailable = true
  end

  def create
    if threads_read_only?
      return redirect_to(
        console_threads_path(thread: params[:thread_key].presence),
        alert: threads_read_only_reason
      )
    end

    prompt = params[:prompt].to_s.strip
    if prompt.blank?
      return redirect_to console_threads_path, alert: "Enter a message to start a thread."
    end

    existing_thread_key = params[:thread_key].to_s.presence
    harness_type = params[:harness_type].presence_in(SUPPORTED_HARNESSES) || "codex"
    thread_key = existing_thread_key || "console:#{SecureRandom.uuid}"
    message_id = "console-msg-#{SecureRandom.uuid}"
    metadata = console_thread_metadata
    action = existing_thread_key ? "reply" : "start_thread"

    unless existing_thread_key
      api_client.create_session(
        thread_key: thread_key,
        harness_type: harness_type,
        metadata: metadata,
        on_harness_conflict: "reject"
      )
    end
    api_client.append_session_messages(
      thread_key: thread_key,
      messages: [
        {
          client_message_id: message_id,
          role: "user",
          parts: [ { type: "text", text: prompt } ],
          metadata: metadata.merge(action: action)
        }
      ]
    )
    api_client.execute_session(
      thread_key: thread_key,
      idempotency_key: "console-#{SecureRandom.uuid}",
      metadata: metadata.merge(action: "execute"),
      input_lines: [ console_input_line(thread_key, message_id, prompt, metadata) ]
    )

    notice = existing_thread_key ? "Message sent." : "Thread started."
    redirect_to console_threads_path(thread: thread_key), notice: notice
  rescue CentaurApiClient::Error => e
    redirect_to console_threads_path, alert: e.message
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
    load_selected_session_summaries(keys)
    @selected_thread_key = @selected_session&.thread_key.to_s
    @selected_messages = selected_messages
    @selected_executions = selected_executions
    @selected_events = selected_events
    @selected_transcript_items = selected_transcript_items
  end

  def empty_thread_state
    @sessions = []
    @selected_session = nil
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

    selected = nil
    if @selected_thread_key.present?
      selected = base_sessions.find { |session| session.thread_key == @selected_thread_key }
      selected ||= session_scope.where(thread_key: @selected_thread_key).first
      selected ||= direct_selected_session(@selected_thread_key)
    end
    selected || @sessions.first
  end

  def auto_select_first_thread?
    params[:thread].blank? && !@starting_new_thread && @query.blank? && @selected_session.present?
  end

  def redirect_to_first_thread
    redirect_to console_threads_path(thread: @selected_session.thread_key)
  end

  def load_selected_session_summaries(loaded_keys)
    return unless @selected_session
    return if loaded_keys.include?(@selected_session.thread_key)

    selected_key = [ @selected_session.thread_key ]
    @latest_messages.merge!(latest_messages_for(selected_key))
    @latest_executions.merge!(latest_executions_for(selected_key))
    @message_counts.merge!(count_records(CentaurSessionMessage, selected_key))
    @execution_counts.merge!(count_records(CentaurSessionExecution, selected_key))
  end

  def direct_selected_session(thread_key)
    return if thread_key.blank?
    return unless thread_key.to_s.start_with?("slack:")

    CentaurSession.where(thread_key: thread_key).first
  end

  def visible_thread_scope
    conditions = [ console_thread_owner_sql ].compact

    slack_owners = slack_thread_owners_for_current_user
    conditions << slack_thread_owner_sql(slack_owners) if slack_owners.any?

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

          credentials.filter_map do |credential|
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
          end.uniq { |owner| [ normalize_key(owner.user_id), normalize_key(owner.team_id) ] }
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

    CentaurSessionMessage
      .where(thread_key: @selected_session.thread_key)
      .order(:created_at, :message_id)
      .limit(MESSAGE_LIMIT)
      .to_a
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

    (message_items + event_items).sort_by do |item|
      [ item[:created_at] || Time.zone.at(0), item[:source] == :event ? 1 : 0 ]
    end
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
    metadata = session.metadata_hash
    summary = metadata["summary"]
    title = metadata["title"].presence ||
      metadata["generated_title"].presence ||
      metadata["summary_title"].presence ||
      metadata["thread_title"].presence ||
      metadata.dig("thread", "title").presence ||
      metadata.dig("summary", "title").presence ||
      (summary if summary.is_a?(String)).presence ||
      metadata["subject"].presence ||
      metadata["issue_title"].presence
    return generated_thread_title(title) if title

    preview = thread_text_preview(@latest_messages[session.thread_key])
    generated = generated_thread_title(preview)
    return generated if generated.present?

    human_thread_key(session.thread_key)
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

  def console_thread_metadata
    {
      platform: "console",
      source: "console",
      actor_email: current_user&.email,
      user_email: current_user&.email,
      user_name: current_user&.email.to_s.split("@").first
    }.compact
  end

  def console_input_line(thread_key, message_id, prompt, metadata)
    JSON.generate(
      type: "user",
      thread_key: thread_key,
      trace_metadata: metadata.merge(action: "execute", client_message_id: message_id),
      message: {
        role: "user",
        content: [ { type: "text", text: prompt } ]
      }
    )
  end

  def api_client
    @api_client ||= self.class.client_factory.call
  end

  def threads_read_only?
    return self.class.read_only_override unless self.class.read_only_override.nil?

    ActiveModel::Type::Boolean.new.cast(ConsoleEnv["THREADS_READ_ONLY"])
  end

  def threads_read_only_reason
    ConsoleEnv["THREADS_READ_ONLY_REASON"].presence || READ_ONLY_REASON
  end
end
