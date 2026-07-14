require "json"
require "net/http"
require "uri"

module SlackDm
  class SyncCredential
    DM_REQUIRED_SCOPES = %w[im:read im:history].freeze
    MPIM_REQUIRED_SCOPES = %w[mpim:read mpim:history].freeze
    PRIVATE_CHANNEL_REQUIRED_SCOPES = %w[groups:read groups:history].freeze
    REQUIRED_SCOPES = (
      DM_REQUIRED_SCOPES + MPIM_REQUIRED_SCOPES + PRIVATE_CHANNEL_REQUIRED_SCOPES
    ).freeze

    AUTH_TEST_ENDPOINT = "https://slack.com/api/auth.test"
    CONVERSATIONS_LIST_ENDPOINT = "https://slack.com/api/conversations.list"
    CONVERSATIONS_MEMBERS_ENDPOINT = "https://slack.com/api/conversations.members"
    CONVERSATIONS_HISTORY_ENDPOINT = "https://slack.com/api/conversations.history"
    CONVERSATIONS_REPLIES_ENDPOINT = "https://slack.com/api/conversations.replies"

    SlackApiError = Class.new(StandardError)

    class << self
      attr_accessor :slack_api_http

      def oauth_app_slug
        ConsoleEnv["SLACK_DM_SYNC_OAUTH_APP_SLUG"].presence || "slack"
      end

      def required_scopes_granted?(scopes)
        supported_conversation_types(scopes).any?
      end

      def supported_conversation_types(scopes)
        granted = Array(scopes)
        types = []
        types << "im" if (DM_REQUIRED_SCOPES - granted).empty?
        types << "mpim" if (MPIM_REQUIRED_SCOPES - granted).empty?
        types << "private_channel" if (PRIVATE_CHANNEL_REQUIRED_SCOPES - granted).empty?
        types
      end
    end

    def initialize(credential, api_client: CentaurApiClient.new, slack_api_http: nil)
      @credential = credential
      @api_client = api_client
      @slack_api_http = slack_api_http || self.class.slack_api_http
      @run_id = "sdms_#{SecureRandom.hex(16)}"
      @messages_fetched = 0
      @replies_fetched = 0
    end

    def call
      auth = slack_api(AUTH_TEST_ENDPOINT)
      home_team_id = auth.fetch("team_id")
      source_user_id = auth["user_id"].presence || @credential.provider_subject.to_s
      checkpoints = load_checkpoints(home_team_id)
      batch = empty_batch(home_team_id, source_user_id)

      conversations = list_conversations
      batch[:run][:conversations_requested] = conversations.length

      conversations.each do |conversation|
        normalize_conversation(conversation, home_team_id, batch)
        normalize_members(conversation, home_team_id, batch)
        sync_history(conversation, home_team_id, checkpoints[conversation.fetch("id")], batch)
        batch[:run][:conversations_synced] += 1
      rescue StandardError => e
        raise if Rails.env.test?

        batch[:run][:conversations_failed] += 1
        Rails.logger.warn do
          "slack DM sync failed for conversation #{conversation['id']}: #{e.class}: #{e.message}"
        end
      end

      batch[:run][:status] = batch[:run][:conversations_failed].positive? ? "partial" : "completed"
      batch[:run][:messages_fetched] = @messages_fetched
      batch[:run][:messages_upserted] = batch[:messages].length
      batch[:run][:replies_fetched] = @replies_fetched
      batch[:run][:replies_upserted] = batch[:messages].count { |message| message[:parent_message_ts].present? }
      batch[:run][:finished] = true
      @api_client.ingest_slack_dm_sync_batch(batch)
    end

    private

    def load_checkpoints(home_team_id)
      response = @api_client.list_slack_dm_sync_checkpoints(
        broker_credential_id: @credential.oid,
        home_team_id: home_team_id
      )
      Array(response["checkpoints"]).to_h do |checkpoint|
        [ checkpoint.fetch("conversation_id"), checkpoint["watermark_ts"] ]
      end
    end

    def empty_batch(home_team_id, source_user_id)
      {
        run: {
          run_id: @run_id,
          mode: "incremental",
          status: "running",
          broker_credential_id: @credential.oid,
          source_user_id: source_user_id,
          home_team_id: home_team_id,
          conversations_requested: 0,
          conversations_synced: 0,
          conversations_failed: 0,
          messages_fetched: 0,
          messages_upserted: 0,
          replies_fetched: 0,
          replies_upserted: 0,
          metadata: {
            oauth_app_slug: @credential.oauth_app&.slug,
            credential_id: @credential.oid
          }
        },
        replace_memberships: true,
        conversations: [],
        members: [],
        messages: [],
        attachments: [],
        checkpoints: []
      }
    end

    def list_conversations
      types = self.class.supported_conversation_types(@credential.scopes)
      raise SlackApiError, "Slack credential has no supported conversation scopes" if types.empty?

      each_page(
        CONVERSATIONS_LIST_ENDPOINT,
        { "types" => types.join(","), "exclude_archived" => "false", "limit" => list_page_size },
        max_pages: list_max_pages
      ).flat_map { |page| Array(page["channels"]) }
    end

    def normalize_conversation(conversation, home_team_id, batch)
      batch[:conversations] << {
        home_team_id: home_team_id,
        conversation_id: conversation.fetch("id"),
        conversation_type: conversation_type(conversation),
        is_archived: conversation["is_archived"] == true,
        is_ext_shared: conversation["is_ext_shared"] == true,
        raw_payload: conversation
      }
    end

    def normalize_members(conversation, home_team_id, batch)
      conversation_id = conversation.fetch("id")
      members = conversation_members(conversation)
      members.each do |member_id|
        batch[:members] << {
          home_team_id: home_team_id,
          conversation_id: conversation_id,
          user_id: member_id,
          is_external: false,
          is_current_member: true,
          raw_payload: { source: "conversations.members" }
        }
      end
    end

    def conversation_members(conversation)
      if conversation["is_im"] && conversation["user"].present?
        members = [ conversation["user"] ]
        members << @credential.provider_subject if @credential.provider_subject.present?
        return members.uniq
      end

      complete = true
      pages = each_page(
        CONVERSATIONS_MEMBERS_ENDPOINT,
        { "channel" => conversation.fetch("id"), "limit" => members_page_size },
        max_pages: members_max_pages
      ) do |_page, truncated|
        complete = false if truncated
      end
      unless complete
        raise SlackApiError,
              "Slack membership pagination truncated for #{conversation.fetch('id')}"
      end

      members = pages.flat_map { |page| Array(page["members"]) }.compact
      members << @credential.provider_subject if @credential.provider_subject.present?
      members.uniq
    end

    def conversation_type(conversation)
      return "mpim" if conversation["is_mpim"]
      return "im" if conversation["is_im"]
      return "private_channel" if conversation["is_private"]

      raise SlackApiError, "Unsupported Slack conversation #{conversation['id']}"
    end

    def sync_history(conversation, home_team_id, checkpoint, batch)
      conversation_id = conversation.fetch("id")
      max_message_ts = checkpoint
      completed = true
      pages = each_page(
        CONVERSATIONS_HISTORY_ENDPOINT,
        history_params(conversation_id, checkpoint),
        max_pages: history_max_pages
      ) do |_page, truncated|
        completed = false if truncated
      end

      pages.each do |page|
        Array(page["messages"]).each do |message|
          @messages_fetched += 1
          max_message_ts = max_slack_ts(max_message_ts, message["ts"])
          normalize_message(message, home_team_id, conversation_id, nil, batch)
          normalize_files(message, home_team_id, conversation_id, batch)
          sync_replies(message, home_team_id, conversation_id, batch) if message["reply_count"].to_i.positive?
        end
      end

      return unless completed

      batch[:checkpoints] << {
        broker_credential_id: @credential.oid,
        home_team_id: home_team_id,
        conversation_id: conversation_id,
        watermark_ts: max_message_ts,
        last_run_id: @run_id
      }
    end

    def sync_replies(root_message, home_team_id, conversation_id, batch)
      thread_ts = root_message["thread_ts"].presence || root_message["ts"]
      pages = each_page(
        CONVERSATIONS_REPLIES_ENDPOINT,
        { "channel" => conversation_id, "ts" => thread_ts, "limit" => replies_page_size },
        max_pages: replies_max_pages
      )

      pages.each do |page|
        Array(page["messages"]).each do |reply|
          next if reply["ts"] == root_message["ts"]

          @replies_fetched += 1
          normalize_message(reply, home_team_id, conversation_id, root_message["ts"], batch)
          normalize_files(reply, home_team_id, conversation_id, batch)
        end
      end
    end

    def normalize_message(message, home_team_id, conversation_id, parent_message_ts, batch)
      ts = message.fetch("ts")
      thread_ts = message["thread_ts"].presence || parent_message_ts
      batch[:messages] << {
        home_team_id: home_team_id,
        conversation_id: conversation_id,
        message_ts: ts,
        thread_ts: thread_ts,
        parent_message_ts: parent_message_ts,
        is_thread_root: thread_ts.present? && thread_ts == ts,
        user_id: message["user"].to_s,
        user_team_id: message["user_team"],
        bot_id: message["bot_id"].to_s,
        message_type: message["type"].presence || "message",
        message_subtype: message["subtype"],
        text: message["text"].to_s,
        permalink: message["permalink"].to_s,
        reply_count: message["reply_count"].to_i,
        reply_users: Array(message["reply_users"]),
        latest_reply_ts: message["latest_reply"],
        thread_refreshed: parent_message_ts.present?,
        raw_payload: message,
        source_run_id: @run_id
      }
    end

    def normalize_files(message, home_team_id, conversation_id, batch)
      Array(message["files"]).each do |file|
        next if file["id"].blank?

        batch[:attachments] << {
          home_team_id: home_team_id,
          conversation_id: conversation_id,
          message_ts: message.fetch("ts"),
          slack_file_id: file.fetch("id"),
          name: file["name"].to_s,
          title: file["title"].to_s,
          mimetype: file["mimetype"].to_s,
          filetype: file["filetype"].to_s,
          size_bytes: file["size"].to_i,
          url_private: file["url_private"].to_s,
          permalink: file["permalink"].to_s,
          raw_payload: file,
          source_run_id: @run_id
        }
      end
    end

    def history_params(conversation_id, checkpoint)
      params = { "channel" => conversation_id, "limit" => history_page_size }
      params["oldest"] = checkpoint if checkpoint.present?
      params["inclusive"] = "false" if checkpoint.present?
      params
    end

    def each_page(endpoint, params, max_pages:)
      pages = []
      cursor = nil
      max_pages.times do |index|
        page = slack_api(endpoint, params.merge("cursor" => cursor).compact)
        pages << page
        cursor = page.dig("response_metadata", "next_cursor").presence
        has_more = cursor.present?
        truncated = has_more && index == max_pages - 1
        yield page, truncated if block_given?
        break unless has_more
        break if truncated
      end
      pages
    end

    def slack_api(endpoint, params = {})
      if @slack_api_http
        return @slack_api_http.call(
          endpoint: endpoint,
          params: params,
          access_token: @credential.access_token
        )
      end

      uri = URI.parse(endpoint)
      uri.query = URI.encode_www_form(params) if params.any?
      request = Net::HTTP::Get.new(uri)
      request["Authorization"] = "Bearer #{@credential.access_token}"
      request["Accept"] = "application/json"

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = slack_timeout
      http.read_timeout = slack_timeout
      response = http.request(request)
      raise SlackApiError, "Slack API rate limited" if response.code.to_i == 429

      parsed = JSON.parse(response.body.to_s)
      raise SlackApiError, "Slack API returned HTTP #{response.code}" unless response.code.to_i.between?(200, 299)
      raise SlackApiError, "Slack API returned #{parsed['error']}" unless parsed["ok"] == true

      parsed
    end

    def max_slack_ts(left, right)
      return right if left.blank?
      return left if right.blank?

      (slack_ts_sort_key(right) <=> slack_ts_sort_key(left)).positive? ? right : left
    end

    def slack_ts_sort_key(value)
      seconds, micros = value.to_s.split(".", 2)
      [ seconds.to_i, micros.to_s.ljust(6, "0")[0, 6].to_i ]
    end

    def slack_timeout = positive_env("SLACK_DM_SYNC_TIMEOUT_SECONDS", 20)
    def list_page_size = positive_env("SLACK_DM_SYNC_LIST_PAGE_SIZE", 200)
    def list_max_pages = positive_env("SLACK_DM_SYNC_LIST_MAX_PAGES", 10)
    def members_page_size = positive_env("SLACK_DM_SYNC_MEMBERS_PAGE_SIZE", 200)
    def members_max_pages = positive_env("SLACK_DM_SYNC_MEMBERS_MAX_PAGES", 10)
    def history_page_size = positive_env("SLACK_DM_SYNC_HISTORY_PAGE_SIZE", 200)
    def history_max_pages = positive_env("SLACK_DM_SYNC_HISTORY_MAX_PAGES", 5)
    def replies_page_size = positive_env("SLACK_DM_SYNC_REPLIES_PAGE_SIZE", 200)
    def replies_max_pages = positive_env("SLACK_DM_SYNC_REPLIES_MAX_PAGES", 5)

    def positive_env(name, default)
      value = ConsoleEnv[name].to_i
      value.positive? ? value : default
    end
  end
end
