require "digest"
require "cgi"
require "json"
require "net/http"
require "uri"

module GoogleDocs
  class SyncCredential
    DRIVE_METADATA_SCOPE = "https://www.googleapis.com/auth/drive.metadata.readonly"
    DRIVE_READONLY_SCOPE = "https://www.googleapis.com/auth/drive.readonly"
    DOCS_READONLY_SCOPE = "https://www.googleapis.com/auth/documents.readonly"
    GOOGLE_DOC_MIME_TYPE = "application/vnd.google-apps.document"
    EXPORT_MIME_TYPE = "text/plain"

    FILES_LIST_ENDPOINT = "https://www.googleapis.com/drive/v3/files"
    DOCS_GET_ENDPOINT = "https://docs.googleapis.com/v1/documents"

    GoogleApiError = Class.new(StandardError)

    class << self
      attr_accessor :google_api_http

      def oauth_app_slug
        ConsoleEnv["GOOGLE_DOCS_SYNC_OAUTH_APP_SLUG"].presence || "google"
      end

      def required_scopes_granted?(scopes)
        granted = Array(scopes)
        granted.include?(DOCS_READONLY_SCOPE) &&
          (granted.include?(DRIVE_METADATA_SCOPE) || granted.include?(DRIVE_READONLY_SCOPE))
      end

      def page_size
        positive_int(ConsoleEnv["GOOGLE_DOCS_SYNC_PAGE_SIZE"], 100)
      end

      def max_pages
        positive_int(ConsoleEnv["GOOGLE_DOCS_SYNC_MAX_PAGES"], 10)
      end

      def chunk_chars
        positive_int(ConsoleEnv["GOOGLE_DOCS_SYNC_CHUNK_CHARS"], 12_000)
      end

      def positive_int(value, default)
        parsed = value.to_i
        parsed.positive? ? parsed : default
      end
    end

    def initialize(credential, api_client: CentaurApiClient.new, google_api_http: nil)
      @credential = credential
      @api_client = api_client
      @google_api_http = google_api_http || self.class.google_api_http
      @run_id = "gdocs_#{SecureRandom.hex(16)}"
      @files_seen = 0
      @docs_fetched = 0
      @docs_failed = 0
      @chunks_upserted = 0
      @max_modified_at = nil
      @truncated = false
    end

    def call
      checkpoint = load_checkpoint
      batch = empty_batch
      modified_after = checkpoint && checkpoint["last_incremental_sync_at"].presence

      files = list_files(modified_after: modified_after)
      batch[:run][:files_seen] = files.length

      files.each do |file|
        normalize_file(file, batch)
        normalize_observation(file, batch)
        sync_content(file, batch)
      rescue StandardError => e
        raise if Rails.env.test?

        @docs_failed += 1
        Rails.logger.warn do
          "Google Docs sync failed for file #{file['id']}: #{e.class}: #{e.message}"
        end
      end

      finish_batch(batch)
      @api_client.ingest_google_docs_sync_batch(batch)
    end

    private

    def load_checkpoint
      @api_client
        .get_google_docs_sync_checkpoint(broker_credential_id: @credential.oid)
        .fetch("checkpoint")
    end

    def empty_batch
      {
        run: {
          run_id: @run_id,
          mode: "incremental",
          status: "running",
          broker_credential_id: @credential.oid,
          provider_subject: @credential.provider_subject.to_s,
          provider_email: @credential.provider_email.to_s,
          files_seen: 0,
          files_upserted: 0,
          docs_fetched: 0,
          docs_upserted: 0,
          chunks_upserted: 0,
          metadata: {
            oauth_app_slug: @credential.oauth_app&.slug,
            credential_id: @credential.oid
          }
        },
        replace_context_documents: true,
        files: [],
        observations: [],
        contents: [],
        context_documents: []
      }
    end

    def list_files(modified_after:)
      files = []
      page_token = nil
      self.class.max_pages.times do |index|
        page = google_api(
          FILES_LIST_ENDPOINT,
          files_list_params(modified_after: modified_after, page_token: page_token)
        )
        page_files = Array(page["files"]).select do |file|
          file["id"].present? && file["mimeType"] == GOOGLE_DOC_MIME_TYPE
        end
        files.concat(page_files)
        page_files.each { |file| track_modified_at(file["modifiedTime"]) }
        page_token = page["nextPageToken"].presence
        if page_token.present? && index == self.class.max_pages - 1
          @truncated = true
        end
        break if page_token.blank?
      end
      @files_seen = files.length
      files
    end

    def files_list_params(modified_after:, page_token:)
      query = [
        "mimeType = '#{GOOGLE_DOC_MIME_TYPE}'",
        "trashed = false"
      ]
      query << "modifiedTime > '#{drive_literal(modified_after)}'" if modified_after.present?
      {
        "q" => query.join(" and "),
        "pageSize" => self.class.page_size,
        "fields" => [
          "nextPageToken,",
          "files(id,name,mimeType,webViewLink,driveId,owners,lastModifyingUser,",
          "capabilities,labelInfo,trashed,explicitlyTrashed,createdTime,modifiedTime,version)"
        ].join,
        "includeItemsFromAllDrives" => "true",
        "supportsAllDrives" => "true",
        "orderBy" => "modifiedTime",
        "pageToken" => page_token
      }.compact
    end

    def normalize_file(file, batch)
      batch[:files] << {
        file_id: file.fetch("id"),
        drive_id: file["driveId"].to_s,
        name: file["name"].to_s,
        mime_type: file["mimeType"].to_s,
        web_view_link: file["webViewLink"].to_s,
        owners: Array(file["owners"]),
        last_modifying_user: file["lastModifyingUser"].is_a?(Hash) ? file["lastModifyingUser"] : {},
        capabilities: file["capabilities"].is_a?(Hash) ? file["capabilities"] : {},
        labels: file["labelInfo"].is_a?(Hash) ? file["labelInfo"] : {},
        trashed: file["trashed"] == true,
        explicitly_trashed: file["explicitlyTrashed"] == true,
        source_created_at: file["createdTime"],
        source_modified_at: file["modifiedTime"],
        source_version: file["version"].to_s,
        raw_payload: file,
        source_run_id: @run_id
      }
    end

    def normalize_observation(file, batch)
      batch[:observations] << {
        broker_credential_id: @credential.oid,
        observed_file_id: file.fetch("id"),
        file_id: file.fetch("id"),
        provider_subject: @credential.provider_subject.to_s,
        provider_email: @credential.provider_email.to_s,
        observed_name: file["name"].to_s,
        observed_mime_type: file["mimeType"].to_s,
        observed_web_view_link: file["webViewLink"].to_s,
        role_hint: role_hint(file),
        permission_ids: [],
        active: true,
        raw_payload: { "source" => "drive.files.list" },
        source_run_id: @run_id
      }
    end

    def sync_content(file, batch)
      doc = docs_get(file.fetch("id"))
      text = docs_text_from_document(doc)
      @docs_fetched += 1
      title = doc["title"].presence || file["name"].to_s
      text_hash = content_hash(text)
      exported_at = Time.current.iso8601
      batch[:contents] << {
        file_id: file.fetch("id"),
        title: title,
        text_content: text,
        text_hash: text_hash,
        export_mime_type: EXPORT_MIME_TYPE,
        exported_at: exported_at,
        source_modified_at: file["modifiedTime"],
        source_version: file["version"].to_s,
        source_run_id: @run_id
      }

      chunks_for(text).each_with_index do |chunk, index|
        chunk_id = format("chunk-%04d", index)
        batch[:context_documents] << context_document(file, title, chunk, chunk_id)
        @chunks_upserted += 1
      end
    rescue StandardError => e
      @docs_failed += 1
      batch[:contents] << {
        file_id: file.fetch("id"),
        title: file["name"].to_s,
        text_hash: "",
        export_mime_type: EXPORT_MIME_TYPE,
        source_modified_at: file["modifiedTime"],
        source_version: file["version"].to_s,
        source_run_id: @run_id,
        last_error: "#{e.class}: #{e.message}"
      }
    end

    def context_document(file, title, body, chunk_id)
      owner = Array(file["owners"]).find { |candidate| candidate.is_a?(Hash) } || {}
      {
        document_id: "google_docs:#{file.fetch('id')}:#{chunk_id}",
        file_id: file.fetch("id"),
        chunk_id: chunk_id,
        title: title,
        body: body,
        url: file["webViewLink"].to_s,
        provider_author_id: owner["permissionId"].to_s,
        provider_author_name: owner["displayName"].presence || owner["emailAddress"].to_s,
        mime_type: file["mimeType"].to_s,
        drive_id: file["driveId"].to_s,
        source_created_at: file["createdTime"],
        source_modified_at: file["modifiedTime"],
        source_version: file["version"].to_s,
        content_hash: content_hash(file.fetch("id"), chunk_id, title, body),
        metadata: {
          source: "google_docs",
          provider_subject: @credential.provider_subject.to_s,
          provider_email: @credential.provider_email.to_s,
          broker_credential_id: @credential.oid
        }
      }
    end

    def finish_batch(batch)
      batch[:run][:files_upserted] = batch[:files].length
      batch[:run][:docs_fetched] = @docs_fetched
      batch[:run][:docs_upserted] = batch[:contents].count { |content| content[:last_error].blank? }
      batch[:run][:chunks_upserted] = @chunks_upserted
      batch[:run][:finished] = true

      successful = @docs_failed.zero? && !@truncated
      batch[:run][:status] = successful ? "completed" : "partial_failed"
      batch[:run][:error_text] = if @truncated
        "Google Docs sync hit max page limit"
      elsif @docs_failed.positive?
        "#{@docs_failed} Google Doc(s) failed"
      else
        ""
      end

      batch[:checkpoint] = {
        broker_credential_id: @credential.oid,
        provider_subject: @credential.provider_subject.to_s,
        provider_email: @credential.provider_email.to_s,
        last_run_id: @run_id,
        last_error: batch[:run][:error_text].to_s,
        metadata: { "page_size" => self.class.page_size, "max_pages" => self.class.max_pages }
      }
      if successful
        now = Time.current.iso8601
        batch[:checkpoint][:last_incremental_sync_at] = (@max_modified_at&.iso8601 || now)
        batch[:checkpoint][:last_full_sync_at] = now
      end
    end

    def docs_get(file_id)
      google_api("#{DOCS_GET_ENDPOINT}/#{CGI.escape(file_id)}", { "includeTabsContent" => "true" })
    end

    def docs_text_from_document(doc)
      if doc["tabs"].is_a?(Array)
        return doc["tabs"].map do |tab|
          extract_text_from_content(tab.dig("documentTab", "body", "content"))
        end.join("\n")
      end

      extract_text_from_content(doc.dig("body", "content"))
    end

    def extract_text_from_content(content)
      Array(content).filter_map do |element|
        if element["paragraph"]
          Array(element.dig("paragraph", "elements")).filter_map do |paragraph_element|
            paragraph_element.dig("textRun", "content")
          end.join
        elsif element["table"]
          Array(element.dig("table", "tableRows")).map do |row|
            Array(row["tableCells"]).map { |cell| extract_text_from_content(cell["content"]) }.join("\n")
          end.join("\n")
        end
      end.join
    end

    def chunks_for(text)
      return [ "" ] if text.blank?

      text.scan(/.{1,#{self.class.chunk_chars}}/m)
    end

    def role_hint(file)
      capabilities = file["capabilities"]
      return "" unless capabilities.is_a?(Hash)
      return "writer" if capabilities["canEdit"] == true
      return "commenter" if capabilities["canComment"] == true
      "reader"
    end

    def track_modified_at(value)
      parsed = Time.iso8601(value.to_s)
      @max_modified_at = parsed if @max_modified_at.nil? || parsed > @max_modified_at
    rescue ArgumentError
      nil
    end

    def drive_literal(value)
      value.to_s.gsub("\\", "\\\\").gsub("'", "\\\\'")
    end

    def content_hash(*parts)
      Digest::SHA256.hexdigest(JSON.generate(parts))
    end

    def google_api(endpoint, params = {})
      uri = URI.parse(endpoint)
      query = URI.decode_www_form(uri.query.to_s) + params.compact.map { |key, value| [ key, value.to_s ] }
      uri.query = URI.encode_www_form(query)
      response = if @google_api_http
        @google_api_http.call(endpoint: endpoint, params: params, access_token: @credential.access_token)
      else
        net_http_get(uri)
      end
      return response if response.is_a?(Hash)

      raise GoogleApiError, "Google API returned invalid response"
    end

    def net_http_get(uri)
      request = Net::HTTP::Get.new(uri)
      request["Accept"] = "application/json"
      request["Authorization"] = "Bearer #{@credential.access_token}"
      response = Net::HTTP.start(uri.host, uri.port, use_ssl: uri.scheme == "https") do |http|
        http.request(request)
      end
      body = response.body.to_s
      parsed = body.present? ? JSON.parse(body) : {}
      return parsed if response.code.to_i.between?(200, 299)

      message = parsed.dig("error", "message") if parsed.is_a?(Hash)
      raise GoogleApiError, message.presence || "Google API returned HTTP #{response.code}"
    rescue JSON::ParserError
      raise GoogleApiError, "Google API returned invalid JSON"
    end
  end
end
