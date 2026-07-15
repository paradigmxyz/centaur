class Console::ThreadsController < ApplicationController
  layout "console"

  # Injectable for tests, mirroring Console::WorkflowsController.
  class_attribute :client_factory, default: -> { CentaurApiClient.new }

  THREAD_LIMIT = 250
  MESSAGE_LIMIT = 80
  EXECUTION_LIMIT = 8
  TRANSCRIPT_EVENT_LIMIT = 80
  PANEL_LIMIT = 4
  THINKING_EVENT_LIMIT = 200
  ACTIVITY_SUMMARY_EVENT_LIMIT = 200
  RAW_TRACE_OUTPUT_LINE_PATTERNS = %w[
    reasoning
    thinking
    tooluse
    tool_use
    toolresult
    tool_result
  ].freeze
  COMPLETED_TRACE_METHOD_PATTERNS = %w[
    item/completed
    item.completed
  ].freeze
  COMPLETED_TRACE_ITEM_PATTERNS = %w[
    commandexecution
    command_execution
    mcptoolcall
    mcp_tool_call
    toolcall
    tool_call
    tooluse
    tool_use
    functioncall
    function_call
    filechange
    file_change
  ].freeze
  TOOL_TRACE_ITEM_TYPES = %w[
    commandExecution
    command_execution
    mcpToolCall
    mcp_tool_call
    toolCall
    tool_call
    toolUse
    tool_use
    functionCall
    function_call
    fileChange
    file_change
  ].freeze
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

  # Pseudo thread key that opens a new-chat composer pane in the split view.
  NEW_PANE_KEY = "new".freeze

  # The composer's model selector, in display order. Each entry pins the
  # harness the choice runs on (wire values match api-rs's HarnessType enum,
  # serde lowercase); the model ids are the ones the bots' --model flags
  # expand to (services/slackbotv2/src/overrides.ts). Amp appears as a plain
  # entry with no model: it picks its own model per turn. `efforts` are the
  # per-turn reasoning efforts the harness accepts for the model (codex only —
  # harness-server discards `reasoning` for claude/amp; enum per
  # crates/harness-server/src/codex.rs, `max` being 5.6-specific).
  ComposerAgent = Struct.new(:value, :label, :harness, :model, :efforts, keyword_init: true)
  CODEX_EFFORTS = [
    %w[minimal Minimal],
    %w[low Low],
    %w[medium Medium],
    %w[high High],
    [ "xhigh", "Extra High" ]
  ].freeze
  # First entry doubles as the default pick (unless the deploy's default-model
  # resolution for its harness names another listed model).
  COMPOSER_AGENTS = [
    ComposerAgent.new(value: "gpt-5.6-sol", label: "GPT-5.6 Sol",
                      harness: "codex", model: "gpt-5.6-sol",
                      efforts: CODEX_EFFORTS + [ %w[max Max] ]),
    ComposerAgent.new(value: "gpt-5.5", label: "GPT-5.5",
                      harness: "codex", model: "gpt-5.5",
                      efforts: CODEX_EFFORTS),
    ComposerAgent.new(value: "claude-opus-4-8", label: "Claude Opus 4.8",
                      harness: "claudecode", model: "claude-opus-4-8", efforts: []),
    ComposerAgent.new(value: "claude-sonnet-4-6", label: "Claude Sonnet 4.6",
                      harness: "claudecode", model: "claude-sonnet-4-6", efforts: []),
    ComposerAgent.new(value: "claude-haiku-4-5", label: "Claude Haiku 4.5",
                      harness: "claudecode", model: "claude-haiku-4-5", efforts: []),
    ComposerAgent.new(value: "claude-fable-5", label: "Claude Fable 5",
                      harness: "claudecode", model: "claude-fable-5", efforts: []),
    ComposerAgent.new(value: "amp", label: "Amp",
                      harness: "amp", model: nil, efforts: [])
  ].freeze

  helper_method :thread_title,
                :thread_source_icon,
                :thread_source_label,
                :thread_harness_label,
                :thread_model_label,
                :thread_user_label,
                :thread_message_text,
                :thread_text_preview,
                :thread_status_classes,
                :composer_agent_choices,
                :composer_default_agent_value,
                :composer_agents_json,
                :thread_execution_active?,
                :thread_owned?

  def index
    @query = params[:q].to_s.strip
    requested_keys = requested_thread_keys
    # "new" is a sentinel pane key: Cmd-clicking the sidebar's New chat adds a
    # composer pane to the split view the same way thread keys add threads. On
    # its own it is just the full-page new-chat screen.
    @new_chat_pane_index = requested_keys.index(NEW_PANE_KEY) if requested_keys.size > 1
    thread_keys = requested_keys - [ NEW_PANE_KEY ]
    @selected_thread_key = thread_keys.first.to_s
    @pane_thread_keys = thread_keys.drop(1)
    @starting_new_thread = params[:new].present? || requested_keys == [ NEW_PANE_KEY ]
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

  # Composer submit: no thread_key starts a new chat (create session + first
  # message + execute), a thread_key sends a follow-up into an existing chat.
  # Both paths talk to api-rs through CentaurApiClient; the transcript itself
  # is still read from the sessions DB by #index after the redirect.
  def create
    thread_key = params[:thread_key].to_s.strip.presence

    prompt = params[:prompt].to_s.strip
    if prompt.blank?
      redirect_to(
        thread_key ? console_threads_path(thread: reply_redirect_keys(thread_key)) : console_threads_path(new: 1),
        alert: "Type a message first."
      )
      return
    end

    thread_key ? reply_to_thread(thread_key, prompt) : start_thread(prompt)
  end

  # Lazily-loaded sidebar thread list, requested by the Turbo Frame in
  # layouts/console.html.erb. Runs the cross-database sessions query out of band
  # so it never blocks the primary page render. Renders only the frame partial
  # (no layout). DB errors leave the list empty via load_console_sidebar_threads.
  def sidebar
    load_console_sidebar_threads
    render partial: "console/threads/sidebar_threads", layout: false
  end

  # Single-panel transcript refresh, polled by thread_poller_controller.js
  # while a turn is running in that panel. Renders only the panel's transcript
  # stream (no layout) so an active thread never drags the rest of the console
  # — other panes, composers, drafts — through a full Turbo visit. Resolves the
  # key through the same readable scope as the page render.
  def panel
    thread_key = params[:thread_key].to_s.strip
    session = readable_thread(thread_key)
    if session.nil?
      head :not_found
      return
    end

    @latest_executions = latest_executions_for([ session.thread_key ])
    panel = thread_panel_for(session)
    active = thread_execution_active?(session.thread_key)
    # The poller stops rescheduling once this header reports the turn is done,
    # after swapping in the final transcript below.
    response.set_header("X-Console-Execution-Active", active.to_s)
    render partial: "console/threads/panel_transcript",
           locals: { items: panel[:transcript_items], active: active },
           layout: false
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_threads_panel_refresh_failed error=#{e.class}: #{e.message}")
    head :service_unavailable
  end

  # Publishes a chat inside the authenticated Console boundary. Publication is
  # stored in Console's own database rather than mutating api-rs session data;
  # the Threads surface remains an observer of the durable transcript.
  def share
    thread_key = params[:thread_key].to_s.strip
    session = owned_thread_scope.where(thread_key: thread_key).first
    if session.nil?
      respond_to do |format|
        format.html { redirect_to console_threads_path, alert: "Chat not found." }
        format.json { render json: { error: "Chat not found." }, status: :not_found }
      end
      return
    end

    ThreadShare.create_or_find_by!(thread_key: session.thread_key) do |share|
      share.created_by = current_user
    end
    share_url = console_threads_url(thread: session.thread_key)
    respond_to do |format|
      format.html do
        redirect_to console_threads_path(thread: session.thread_key)
      end
      format.json { render json: { url: share_url } }
    end
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_thread_share_failed error=#{e.class}: #{e.message}")
    respond_to do |format|
      format.html { redirect_to console_threads_path, alert: "Could not share the chat." }
      format.json { render json: { error: "Could not share the chat." }, status: :service_unavailable }
    end
  end

  private

  def api_client
    @api_client ||= client_factory.call
  end

  # Whether the thread's newest execution is still running — drives the
  # transcript's thinking indicator and the while-running auto-refresh.
  def thread_execution_active?(thread_key)
    execution = @latest_executions&.[](thread_key)
    execution.present? && %w[queued running executing].include?(execution.status.to_s)
  end

  # Public and explicitly shared chats are read-only for non-owners. Keep the
  # composer and write endpoint tied to the original owner scope.
  def thread_owned?(session)
    @thread_owned ||= {}
    @thread_owned.fetch(session.thread_key) do |thread_key|
      @thread_owned[thread_key] = owned_thread_scope.where(thread_key: thread_key).exists?
    end
  end

  # Selector options as [label, value] pairs, the deploy's default model
  # first (pre-checked in the menu). The default comes from the same
  # env/config resolution the thread header uses, so the composer never
  # claims a default the sandbox would not actually run.
  def composer_agent_choices
    default_value = composer_default_agent_value
    COMPOSER_AGENTS
      .sort_by.with_index { |agent, index| agent.value == default_value ? -1 : index }
      .map { |agent| [ agent.label, agent.value ] }
  end

  def composer_default_agent_value
    default = default_model_for_harness(COMPOSER_AGENTS.first.harness)
    COMPOSER_AGENTS.find { |agent| agent.value == default }&.value ||
      COMPOSER_AGENTS.first.value
  end

  # Per-agent metadata the picker script needs to rebuild the effort submenu
  # when the model changes: { value => { label:, efforts: [[value, label]] } }.
  def composer_agents_json
    COMPOSER_AGENTS.to_h do |agent|
      [ agent.value, { label: agent.label, efforts: agent.efforts } ]
    end.to_json
  end

  def composer_effort_param(agent)
    effort = params[:effort].to_s.strip
    return nil if effort.blank?

    agent.efforts.map(&:first).include?(effort) ? effort : nil
  end

  def composer_agent_for(raw)
    value = raw.to_s.strip
    value = composer_default_agent_value if value.blank?
    COMPOSER_AGENTS.find { |agent| agent.value == value }
  end

  def start_thread(prompt)
    agent = composer_agent_for(params[:model])
    if agent.nil?
      redirect_to console_threads_path(new: 1),
                  alert: "Unknown model #{params[:model].to_s.inspect}."
      return
    end

    thread_key = "console:#{SecureRandom.uuid}"
    api_client.create_session(
      thread_key: thread_key,
      harness_type: agent.harness,
      metadata: console_actor_metadata.merge(agent.model.present? ? { model: agent.model } : {})
    )
    send_prompt(thread_key, prompt, model: agent.model, effort: composer_effort_param(agent))
    # A new-chat pane in a split view swaps the sentinel for the created
    # thread so the other panes stay open.
    open_keys = params[:open_threads].to_s.split(",").map(&:strip).reject(&:blank?)
    redirect_keys = open_keys.include?(NEW_PANE_KEY) ?
      open_keys.map { |key| key == NEW_PANE_KEY ? thread_key : key } : [ thread_key ]
    redirect_to console_threads_path(thread: redirect_keys.uniq.first(PANEL_LIMIT).join(","))
  rescue CentaurApiClient::Error => e
    redirect_to console_threads_path(new: 1), alert: "Could not start the chat: #{e.message}"
  end

  def reply_to_thread(thread_key, prompt)
    # Resolve through the owner scope so a crafted thread_key cannot post into
    # another user's chat, even when public or shared read access is allowed.
    session = owned_thread_scope.where(thread_key: thread_key).first
    if session.nil?
      redirect_to console_threads_path, alert: "Chat not found."
      return
    end

    send_prompt(session.thread_key, prompt, model: reply_model_for(session))
    redirect_to console_threads_path(thread: reply_redirect_keys(session.thread_key))
  rescue CentaurApiClient::Error => e
    redirect_to console_threads_path(thread: reply_redirect_keys(thread_key)),
                alert: "Could not send the message: #{e.message}"
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_threads_reply_lookup_failed error=#{e.class}: #{e.message}")
    redirect_to console_threads_path, alert: "Chat database is unavailable."
  end

  # Append persists the turn in conversation history; execute runs it. The
  # shared client_message_id lets api-rs dedupe the copy of the message the
  # harness echoes back.
  def send_prompt(thread_key, prompt, model: nil, effort: nil)
    message_id = SecureRandom.uuid

    api_client.append_session_messages(
      thread_key: thread_key,
      messages: [
        {
          client_message_id: message_id,
          role: "user",
          parts: [ { type: "text", text: prompt } ],
          metadata: console_actor_metadata
        }
      ]
    )

    execute_metadata = console_actor_metadata.merge(action: "execute")
    execute_metadata[:model] = model if model.present?
    execute_metadata[:reasoning] = effort if effort.present?
    api_client.execute_session(
      thread_key: thread_key,
      idempotency_key: SecureRandom.uuid,
      metadata: execute_metadata,
      input_lines: [
        composer_input_line(
          thread_key, prompt,
          model: model, effort: effort, client_message_id: message_id
        )
      ]
    )
  end

  # One blocks-protocol user line, the shape harness-server parses from
  # execute's input_lines. `model` is honored by every harness; omitted (e.g.
  # for Amp) the harness runs its own default. `reasoning` is the per-turn
  # codex effort; other harnesses discard it, and validation upstream only
  # accepts it for codex models anyway.
  def composer_input_line(thread_key, prompt, model:, effort:, client_message_id:)
    line = {
      type: "user",
      thread_key: thread_key,
      client_user_message_id: client_message_id,
      trace_metadata: { action: "execute", source: "console" },
      message: {
        role: "user",
        content: [
          { type: "text", text: console_requester_context },
          { type: "text", text: prompt }
        ]
      }
    }
    line[:model] = model if model.present?
    line[:reasoning] = effort if effort.present?
    line.to_json
  end

  # Resolve the signed-in human through the same Slack profile custom-field
  # path as slackbotv2, falling back to their Console display name/email. Keep
  # this separate from the persisted prompt: it is harness execution context.
  def console_requester_context
    github_identity = SlackRequesterIdentity.resolve(
      user_ids: slack_thread_owners_for_current_user.map(&:user_id)
    )
    prompted_by = github_identity.handle.presence ||
      (current_user&.name.to_s.strip.presence || current_user&.email.to_s)
    github_status = github_identity.handle.present? ?
      "GitHub handle source: #{github_identity.source}\nGitHub handle verified: yes" :
      "GitHub handle verified: no\nGitHub handle unavailable reason: #{github_identity.reason}"
    <<~CONTEXT.strip
      # Requester Context

      The Console user who prompted this turn is #{prompted_by}.

      ## GitHub PR Attribution

      If you create a GitHub PR for this request, the PR body MUST contain this standalone line:
      Prompted by: #{prompted_by}

      #{github_status}

      The user message follows in the next content block.
      ---
    CONTEXT
  end

  # Follow-ups reuse the model the chat has been running on (mirrors the
  # display resolution in thread_model_label, minus the upcasing): last
  # execution's recorded model, session metadata, then the deploy default.
  def reply_model_for(session)
    recorded_model(latest_executions_for([ session.thread_key ])[session.thread_key]&.metadata) ||
      recorded_model(session.metadata_hash) ||
      default_model_for_harness(session.harness_type.to_s)
  end

  # Keeps split-view panes open across a composer submit: the form carries the
  # page's full ?thread= list, and the redirect re-orders it so the posted
  # thread stays primary. Unowned keys are filtered again by #index on render.
  def reply_redirect_keys(thread_key)
    open_keys = params[:open_threads].to_s.split(",").map(&:strip).reject(&:blank?)
    ([ thread_key ] + open_keys).uniq.first(PANEL_LIMIT).join(",")
  end

  def console_actor_metadata
    email = current_user&.email.to_s
    {
      platform: "console",
      source: "console",
      user_email: email,
      actor_email: email
    }
  end

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
      # Resolve the key through the readable scope so a directly linked chat only
      # loads when it is visible to the current user. base_sessions is capped at
      # THREAD_LIMIT, so this also recovers a visible thread beyond that window.
      selected ||= session_scope.where(thread_key: @selected_thread_key).first
      selected ||= explicitly_shared_thread(@selected_thread_key)
      # A directly requested key outside the readable scope renders as 404 rather
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

  # Extra split-view panes resolve through the same readable scope as the
  # primary thread. Inaccessible keys are dropped silently.
  def resolve_pane_sessions(session_scope, base_sessions)
    keys = @pane_thread_keys - [ @selected_session&.thread_key ]
    keys.filter_map do |key|
      base_sessions.find { |session| session.thread_key == key } ||
        session_scope.where(thread_key: key).first ||
        explicitly_shared_thread(key)
    end
  end

  def build_thread_panels
    sessions = ([ @selected_session ] + Array(@pane_sessions)).compact
      .uniq(&:thread_key)
      .first(PANEL_LIMIT)
    panels = if sessions.empty?
      []
    else
      # Build the primary panel last so the @selected_* thread state (used by
      # the page header and mention-resolution memos) ends on the primary
      # thread.
      extra_panels = sessions.drop(1).map { |session| thread_panel_for(session) }
      [ thread_panel_for(sessions.first) ] + extra_panels
    end

    if @new_chat_pane_index && panels.any?
      panels.insert(
        [ @new_chat_pane_index, panels.size ].min,
        { new_chat: true, thread_key: NEW_PANE_KEY, session: nil, transcript_items: [] }
      )
    end
    panels
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
      writable: thread_owned?(session),
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
    thread_scope(include_public_slack: true)
  end

  def owned_thread_scope
    thread_scope(include_public_slack: false)
  end

  def thread_scope(include_public_slack:)
    slack_owners = slack_thread_owners_for_current_user
    conditions = [
      console_thread_owner_sql,
      (slack_thread_owner_sql(slack_owners) if slack_owners.any?)
    ].compact
    if include_public_slack && CentaurSession.public_slack_threads_enabled?
      public_slack_sql = CentaurSession.public_slack_channel_sql
      conditions << public_slack_sql if public_slack_sql
    end

    return CentaurSession.where("1=0") if conditions.empty?

    CentaurSession.where(conditions.map { |condition| "(#{condition})" }.join(" OR "))
  end

  def readable_thread(thread_key)
    return if thread_key.blank?

    visible_thread_scope.where(thread_key: thread_key).first || explicitly_shared_thread(thread_key)
  end

  def explicitly_shared_thread(thread_key)
    return unless ThreadShare.exists?(thread_key: thread_key)

    CentaurSession.where(thread_key: thread_key).first
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
  # "reasoning" carrying the full accumulated thinking text; tool activity
  # arrives as completed command/tool items. Claude Code stream-json persists
  # each assistant API message whose content can include "thinking" and
  # "tool_use" blocks. The SQL LIKE filter keeps the query from paging through
  # the whole firehose; exact matching happens here.
  def selected_thinking_items
    return [] unless @selected_session

    items = CentaurSessionEvent
      .where(thread_key: @selected_session.thread_key)
      .where(event_type: "session.output.line")
      .where(trace_output_line_filter_sql, *trace_output_line_filter_values)
      .order(event_id: :desc)
      .limit(THINKING_EVENT_LIMIT)
      .to_a
      .reverse
      .filter_map { |event| thinking_transcript_item(event) }

    apply_activity_summaries(compact_trace_items(items))
  end

  # api-rs's activity-summary worker condenses harness output into short
  # first-person status lines persisted as session.activity_summary events,
  # each pointing at the output-line event that triggered it via
  # source_event_id. A summary belongs to the latest trace item at or before
  # its source line, so each disclosure's collapsed preview shows the newest
  # status generated during that block; items no summary covers keep the
  # raw-text fallback rendered by the transcript partial.
  def apply_activity_summaries(items)
    anchored = items.select { |item| item[:event_id] }
    return items if anchored.empty?

    selected_activity_summaries.each do |event|
      payload = event.payload_hash
      summary = payload["summary"].to_s.strip
      source_event_id = payload["source_event_id"]
      next if summary.blank? || source_event_id.nil?

      item = anchored.reverse_each.find { |candidate| candidate[:event_id] <= source_event_id.to_i }
      item[:summary] = summary if item
    end
    items
  end

  def selected_activity_summaries
    return [] unless @selected_session

    CentaurSessionEvent
      .where(thread_key: @selected_session.thread_key)
      .where(event_type: "session.activity_summary")
      .order(event_id: :desc)
      .limit(ACTIVITY_SUMMARY_EVENT_LIMIT)
      .to_a
      .reverse
  end

  def thinking_transcript_item(event)
    line = event.payload
    return nil unless line.is_a?(String)

    value = JSON.parse(line)
    return nil unless value.is_a?(Hash)

    trace = reasoning_trace(value) || claude_thinking_trace(value) || tool_trace(value)
    return nil unless trace

    {
      role: "thinking",
      label: trace[:label],
      align: :start,
      text: trace[:text],
      trace_kind: trace[:kind] || "thinking",
      commands: trace[:commands],
      tools: trace[:tools],
      execution_id: event.execution_id,
      event_id: event.event_id,
      created_at: event.created_at,
      source: :thinking
    }
  rescue JSON::ParserError
    nil
  end

  def compact_trace_items(items)
    grouped = []
    command_group = []

    flush_command_group = lambda do
      grouped << command_trace_group(command_group) if command_group.any?
      command_group = []
    end

    items.each do |item|
      if item[:trace_kind] == "command" &&
          (command_group.empty? || same_trace_group?(command_group.last, item))
        command_group << item
      else
        flush_command_group.call
        item[:trace_kind] == "command" ? command_group << item : grouped << item
      end
    end

    flush_command_group.call
    grouped
  end

  def same_trace_group?(left, right)
    left_execution = left[:execution_id].presence
    right_execution = right[:execution_id].presence
    return left_execution == right_execution if left_execution && right_execution

    # Older imported fixtures can lack execution ids. Keep immediately adjacent
    # command traces together, but avoid merging activity from distinct turns.
    left_time = left[:created_at]
    right_time = right[:created_at]
    left_time.present? && right_time.present? && (right_time - left_time).abs <= 5.minutes
  end

  def command_trace_group(items)
    commands = items.flat_map { |item| Array(item[:commands]) }
    failed_count = commands.count { |command| command[:failed] }
    command_count = commands.length

    {
      role: "thinking",
      label: "Ran #{pluralized_count(command_count, "command")}",
      failed_label: failed_count.positive? ? "#{failed_count} failed" : nil,
      align: :start,
      text: command_group_text(commands),
      trace_kind: "commands",
      commands: commands,
      execution_id: items.first[:execution_id],
      event_id: items.first[:event_id],
      created_at: items.first[:created_at],
      source: :thinking
    }
  end

  def command_group_text(commands)
    commands.map do |command|
      [
        "$ #{command[:command]}",
        ("Status: #{command[:status]}" if command[:status].present?),
        ("Exit code: #{command[:exit_code]}" if command[:exit_code].present?),
        command[:output]
      ].compact.join("\n")
    end.join("\n\n").strip
  end

  def pluralized_count(count, singular)
    "#{count} #{singular}#{count == 1 ? "" : "s"}"
  end

  def trace_output_line_filter_sql
    @trace_output_line_filter_sql ||= begin
      raw = RAW_TRACE_OUTPUT_LINE_PATTERNS.map { "lower(payload::text) LIKE ?" }.join(" OR ")
      completed_methods =
        COMPLETED_TRACE_METHOD_PATTERNS.map { "lower(payload::text) LIKE ?" }.join(" OR ")
      completed_items =
        COMPLETED_TRACE_ITEM_PATTERNS.map { "lower(payload::text) LIKE ?" }.join(" OR ")
      "(#{raw}) OR ((#{completed_methods}) AND (#{completed_items}))"
    end
  end

  def trace_output_line_filter_values
    @trace_output_line_filter_values ||= begin
      patterns =
        RAW_TRACE_OUTPUT_LINE_PATTERNS +
        COMPLETED_TRACE_METHOD_PATTERNS +
        COMPLETED_TRACE_ITEM_PATTERNS
      patterns.map { |pattern| "%#{pattern}%" }
    end
  end

  def reasoning_trace(value)
    text = reasoning_event_text(value)
    return nil if text.blank?

    { label: "Thinking", text: text }
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
  def claude_thinking_trace(value)
    text = claude_thinking_text(value)
    return nil if text.blank?

    { label: "Thinking", text: text }
  end

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

  def tool_trace(value)
    completed_item_trace(value) || claude_tool_use_trace(value) || claude_tool_result_trace(value)
  end

  def completed_item_trace(value)
    method = (value["method"] || value["type"]).to_s.tr("/", ".")
    return nil unless method == "item.completed"

    item = value.dig("params", "item") || value["item"]
    return nil unless item.is_a?(Hash)

    case item["type"].to_s
    when "commandExecution", "command_execution"
      command_execution_trace(item)
    when *TOOL_TRACE_ITEM_TYPES
      generic_tool_item_trace(item)
    end
  end

  def command_execution_trace(item)
    command = first_present(item["command"], item["cmd"])
    output = first_present(
      item["aggregatedOutput"],
      item["aggregated_output"],
      item["output"],
      item["stdout"],
      item["stderr"]
    )
    exit_code = first_present(item["exitCode"], item["exit_code"])
    status = first_present(item["status"], exit_code.present? ? "completed" : nil)

    sections = []
    sections << "Status: #{status}" if status.present?
    sections << "Exit code: #{exit_code}" if exit_code.present?
    sections << markdown_code_block(command, language: shell_language_for_command(command)) if command.present?
    sections << "Output:\n\n#{markdown_code_block(output, language: "text")}" if output.present?

    text = sections.compact.join("\n\n").strip
    return nil if text.blank?

    {
      kind: "command",
      label: "Ran 1 command",
      text: text,
      commands: [
        {
          command: command.to_s,
          output: output.to_s,
          exit_code: exit_code,
          status: status,
          failed: command_failed?(status, exit_code)
        }
      ]
    }
  end

  def command_failed?(status, exit_code)
    status.to_s.match?(/\A(?:failed|error|cancelled|timed_out)\z/i) ||
      (exit_code.present? && exit_code.to_i != 0)
  end

  def generic_tool_item_trace(item)
    label = trace_label_for_item(item)
    name = first_present(item["name"], item["tool"], item["toolName"], item["tool_name"])
    input = item["input"] || item["arguments"] || item["args"]
    output = item["output"] || item["result"]

    sections = []
    sections << "Status: #{item["status"]}" if item["status"].present?
    sections << "Name: #{name}" if name.present?
    sections << "Input:\n\n#{markdown_code_block(pretty_json(input))}" if input.present?
    sections << "Output:\n\n#{markdown_code_block(pretty_json(output))}" if output.present?

    text = sections.compact.join("\n\n").strip
    return nil if text.blank?

    { label: label, text: text }
  end

  def claude_tool_use_trace(value)
    return nil unless value["type"].to_s == "assistant"

    content = message_content(value)
    return nil unless content.is_a?(Array)

    traces = content.filter_map do |part|
      next unless part.is_a?(Hash) && part["type"].to_s == "tool_use"

      name = first_present(part["name"], part["tool"], "tool")
      input = part["input"] || part["arguments"]
      [
        "Use #{name}",
        ("Input:\n\n#{markdown_code_block(pretty_json(input))}" if input.present?)
      ].compact.join("\n\n")
    end

    text = traces.join("\n\n").strip
    return nil if text.blank?

    { label: traces.size == 1 ? "Tool call" : "Tool calls", text: text }
  end

  def claude_tool_result_trace(value)
    return nil unless %w[user tool].include?(value["type"].to_s)

    content = message_content(value)
    return nil unless content.is_a?(Array)

    traces = content.filter_map do |part|
      next unless part.is_a?(Hash)
      next unless part["type"].to_s == "tool_result" || part["tool_use_id"].present?

      body = first_present(part["content"], part["text"], part["result"])
      next if body.blank?

      [
        ("Tool use: #{part["tool_use_id"]}" if part["tool_use_id"].present?),
        markdown_code_block(pretty_json(body), language: "text")
      ].compact.join("\n\n")
    end

    text = traces.join("\n\n").strip
    return nil if text.blank?

    { label: traces.size == 1 ? "Tool result" : "Tool results", text: text }
  end

  def message_content(value)
    message = value["message"]
    message.is_a?(Hash) ? message["content"] : value["content"]
  end

  def trace_label_for_item(item)
    case item["type"].to_s
    when "fileChange", "file_change" then "File change"
    when "mcpToolCall", "mcp_tool_call" then "Tool call"
    else "Tool call"
    end
  end

  def markdown_code_block(value, language: nil)
    body = value.to_s.rstrip
    return nil if body.blank?

    fence = "```"
    fence += "`" while body.include?(fence)
    "#{fence}#{language}\n#{body}\n#{fence}"
  end

  def pretty_json(value)
    case value
    when String
      value
    else
      JSON.pretty_generate(value)
    end
  rescue JSON::GeneratorError
    value.to_s
  end

  def shell_language_for_command(command)
    command.to_s.match?(/\A(?:SELECT|WITH|INSERT|UPDATE|DELETE)\b/i) ? "sql" : "sh"
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
