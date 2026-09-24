require "cgi"
require "digest"
require "json"
require "net/http"
require "tempfile"
require "uri"

module GoogleDocs
  class SyncCredential
    DRIVE_METADATA_SCOPE = "https://www.googleapis.com/auth/drive.metadata.readonly"
    DRIVE_READONLY_SCOPE = "https://www.googleapis.com/auth/drive.readonly"
    DOCS_READONLY_SCOPE = "https://www.googleapis.com/auth/documents.readonly"
    GOOGLE_DOC_MIME_TYPE = "application/vnd.google-apps.document"
    PDF_MIME_TYPE = "application/pdf"
    EXPORT_MIME_TYPE = "text/plain"
    NAME_MAX_BYTES = 1_024
    USER_CORPUS = "user"
    FETCH_READ_TIMEOUT_SECONDS = 60
    PDF_BACKFILL_VERSION = 1
    MAX_PDF_BYTES = 50 * 1024 * 1024

    FILES_LIST_ENDPOINT = "https://www.googleapis.com/drive/v3/files"
    CHANGES_LIST_ENDPOINT = "https://www.googleapis.com/drive/v3/changes"
    START_PAGE_TOKEN_ENDPOINT = "https://www.googleapis.com/drive/v3/changes/startPageToken"
    DOCS_GET_ENDPOINT = "https://docs.googleapis.com/v1/documents"

    FILE_FIELDS = [
      "id", "name", "mimeType", "webViewLink", "driveId", "owners", "lastModifyingUser",
      "capabilities", "labelInfo", "trashed", "explicitlyTrashed", "createdTime", "modifiedTime",
      "version"
    ].join(",")

    class GoogleApiError < StandardError; end
    class InvalidPageTokenError < GoogleApiError; end
    class PdfContentError < StandardError; end
    class PdfTooLargeError < PdfContentError; end
    class PdfExtractionError < PdfContentError; end

    NETWORK_ERRORS = [
      EOFError,
      Errno::ECONNREFUSED,
      Errno::ECONNRESET,
      Errno::EHOSTUNREACH,
      Errno::EPIPE,
      Errno::ETIMEDOUT,
      SocketError,
      Timeout::Error
    ].freeze

    class << self
      attr_accessor :google_api_http

      def oauth_app_slug
        ConsoleEnv["GOOGLE_DOCS_SYNC_OAUTH_APP_SLUG"].presence || "google"
      end

      def required_scopes_granted?(scopes)
        granted = Array(scopes)
        granted.include?(DRIVE_READONLY_SCOPE) ||
          (granted.include?(DRIVE_METADATA_SCOPE) && granted.include?(DOCS_READONLY_SCOPE))
      end

      def syncable?(credential, oauth_app_slug: nil)
        credential.present? && !credential.dead? && credential.access_token.present? &&
          credential.oauth_app&.provider == Oauth::Providers::Google::KEY &&
          credential.oauth_app.enabled? && required_scopes_granted?(credential.scopes) &&
          (oauth_app_slug.nil? || credential.oauth_app.slug == oauth_app_slug)
      end

      def page_size
        positive_int(ConsoleEnv["GOOGLE_DOCS_SYNC_PAGE_SIZE"], 100)
      end

      def chunk_chars
        positive_int(ConsoleEnv["GOOGLE_DOCS_SYNC_CHUNK_CHARS"], 12_000)
      end

      def pdf_access?(credential)
        Array(credential.scopes).include?(DRIVE_READONLY_SCOPE)
      end

      def pdf_backfill_version(checkpoint)
        metadata = checkpoint.to_h.fetch("metadata", {})
        metadata.is_a?(Hash) ? metadata.fetch("pdf_backfill_version", 0).to_i : 0
      end

      def pdf_backfill_required?(credential, checkpoint)
        GoogleDocs::Config.pdf_indexing_enabled? && pdf_access?(credential) &&
          pdf_backfill_version(checkpoint) < PDF_BACKFILL_VERSION
      end

      def pdf_backfill_reset_required?(checkpoint)
        !GoogleDocs::Config.pdf_indexing_enabled? && pdf_backfill_version(checkpoint).positive?
      end

      def positive_int(value, default)
        parsed = value.to_i
        parsed.positive? ? parsed : default
      end
    end

    attr_reader :credential

    def initialize(credential, google_api_http: nil, pdf_text_extractor: nil, max_pdf_bytes: MAX_PDF_BYTES)
      @credential = credential
      @google_api_http = google_api_http || self.class.google_api_http
      @pdf_text_extractor = pdf_text_extractor || GoogleDocs::PdfTextExtractor.method(:extract)
      @max_pdf_bytes = max_pdf_bytes
    end

    def user_start_page_token
      google_api(
        START_PAGE_TOKEN_ENDPOINT,
        "supportsAllDrives" => "true"
      ).fetch("startPageToken")
    rescue KeyError
      raise GoogleApiError, "Google Drive returned no start page token"
    end

    def list_user_files_page(page_token: nil)
      list_files_page(mime_types: eligible_mime_types, page_token: page_token)
    end

    def list_user_pdfs_page(page_token: nil)
      list_files_page(mime_types: [ PDF_MIME_TYPE ], page_token: page_token)
    end

    def list_files_page(mime_types:, page_token: nil)
      google_api(
        FILES_LIST_ENDPOINT,
        {
          "q" => "(#{mime_types.map { |mime_type| "mimeType = '#{mime_type}'" }.join(' or ')}) " \
            "and trashed = false",
          "pageSize" => self.class.page_size,
          "fields" => "nextPageToken,files(#{FILE_FIELDS})",
          "corpora" => USER_CORPUS,
          "includeItemsFromAllDrives" => "true",
          "supportsAllDrives" => "true",
          "orderBy" => "modifiedTime,name",
          "pageToken" => page_token
        }.compact
      )
    end

    def list_user_changes_page(page_token:)
      google_api(
        CHANGES_LIST_ENDPOINT,
        {
          "pageToken" => page_token,
          "pageSize" => self.class.page_size,
          "fields" => "nextPageToken,newStartPageToken," \
            "changes(changeType,driveId,fileId,removed,file(#{FILE_FIELDS}))",
          "includeItemsFromAllDrives" => "true",
          "includeRemoved" => "true",
          "supportsAllDrives" => "true",
          "spaces" => "drive"
        }
      )
    end

    def eligible_file?(file)
      file.is_a?(Hash) && file["id"].present? &&
        eligible_mime_types.include?(file["mimeType"]) && file["trashed"] != true
    end

    def file_payload(file, run_id: nil)
      {
        file_id: file.fetch("id"),
        drive_id: file["driveId"].to_s,
        name: truncated_name(file),
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
        source_version: source_version(file),
        raw_payload: file,
        source_run_id: run_id
      }
    end

    def observation_payload(file, run_id: nil, source:)
      {
        broker_credential_id: credential.oid,
        observed_file_id: file.fetch("id"),
        file_id: file.fetch("id"),
        provider_subject: credential.provider_subject.to_s,
        provider_email: credential.provider_email.to_s,
        observed_name: truncated_name(file),
        observed_mime_type: file["mimeType"].to_s,
        observed_web_view_link: file["webViewLink"].to_s,
        role_hint: role_hint(file),
        permission_ids: [],
        active: true,
        raw_payload: { "source" => source },
        source_run_id: run_id
      }
    end

    def observation_deactivation(file_id)
      {
        broker_credential_id: credential.oid,
        observed_file_id: file_id
      }
    end

    def content_version(file)
      {
        file_id: file.fetch("id"),
        source_version: source_version(file)
      }
    end

    def document_batch(file)
      title, text, export_mime_type = if file["mimeType"] == PDF_MIME_TYPE
        [ file["name"].to_s, pdf_text(file), PDF_MIME_TYPE ]
      else
        doc = google_api(
          "#{DOCS_GET_ENDPOINT}/#{CGI.escape(file.fetch('id'))}",
          "includeTabsContent" => "true"
        )
        [ doc["title"].presence || file["name"].to_s, docs_text_from_document(doc), EXPORT_MIME_TYPE ]
      end
      contents = [
        {
          file_id: file.fetch("id"),
          title: title,
          text_content: text,
          text_hash: content_hash(text),
          export_mime_type: export_mime_type,
          exported_at: Time.current.iso8601,
          source_modified_at: file["modifiedTime"],
          source_version: source_version(file)
        }
      ]
      context_documents = chunks_for(text).each_with_index.map do |chunk, index|
        context_document(file, title, chunk, format("chunk-%04d", index))
      end
      {
        contents: contents,
        context_documents: context_documents,
        replace_context_documents: true
      }
    end

    def content_failure_batch(file, error)
      {
        contents: [
          {
            file_id: file.fetch("id"),
            title: file["name"].to_s,
            text_content: "",
            text_hash: content_hash(""),
            export_mime_type: file["mimeType"].to_s,
            exported_at: Time.current.iso8601,
            source_modified_at: file["modifiedTime"],
            source_version: source_version(file),
            last_error: "#{error.class}: #{error.message}".byteslice(0, 1_000)
          }
        ],
        context_documents: [],
        replace_context_documents: true
      }
    end

    private

    def eligible_mime_types
      mime_types = [ GOOGLE_DOC_MIME_TYPE ]
      if GoogleDocs::Config.pdf_indexing_enabled? && self.class.pdf_access?(credential)
        mime_types << PDF_MIME_TYPE
      end
      mime_types
    end

    def pdf_text(file)
      Tempfile.create([ "google-drive-pdf-", ".pdf" ], binmode: true) do |tempfile|
        google_download(
          "#{FILES_LIST_ENDPOINT}/#{CGI.escape(file.fetch('id'))}",
          tempfile,
          "alt" => "media",
          "supportsAllDrives" => "true"
        )
        tempfile.flush
        @pdf_text_extractor.call(tempfile.path).to_s
      end
    rescue GoogleDocs::PdfTextExtractor::Error => error
      raise PdfExtractionError, error.message
    end

    def truncated_name(file)
      name = file["name"].to_s
      return name if name.bytesize <= NAME_MAX_BYTES

      name.byteslice(0, NAME_MAX_BYTES).scrub("")
    end

    def source_version(file)
      file["version"].presence || file["modifiedTime"].to_s
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
        source_version: source_version(file),
        content_hash: content_hash(file.fetch("id"), chunk_id, title, body),
        metadata: { source: "google_docs" }
      }
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
      return [] if text.blank?

      text.scan(/.{1,#{self.class.chunk_chars}}/m)
    end

    def role_hint(file)
      capabilities = file["capabilities"]
      return "" unless capabilities.is_a?(Hash)
      return "writer" if capabilities["canEdit"] == true
      return "commenter" if capabilities["canComment"] == true

      "reader"
    end

    def content_hash(*parts)
      Digest::SHA256.hexdigest(JSON.generate(parts))
    end

    def google_api(endpoint, params = {})
      response = if @google_api_http
        @google_api_http.call(endpoint: endpoint, params: params, access_token: credential.access_token)
      else
        net_http_get(endpoint, params)
      end
      return response if response.is_a?(Hash)

      raise GoogleApiError, "Google API returned invalid response"
    rescue *NETWORK_ERRORS => error
      raise GoogleApiError, "Google API network request failed: #{error.class}"
    end

    def google_download(endpoint, destination, params)
      if @google_api_http
        response = @google_api_http.call(
          endpoint: endpoint,
          params: params,
          access_token: credential.access_token
        )
        raise GoogleApiError, "Google API returned invalid file content" unless response.is_a?(String)

        write_pdf_chunk(destination, response, 0)
      else
        net_http_download(endpoint, destination, params)
      end
    rescue *NETWORK_ERRORS => error
      raise GoogleApiError, "Google API network request failed: #{error.class}"
    end

    def net_http_get(endpoint, params)
      response = HttpClient.new(read_timeout: FETCH_READ_TIMEOUT_SECONDS).get(
        endpoint,
        params: params,
        headers: { "Authorization" => "Bearer #{credential.access_token}" }
      )
      parsed = response.json
      return parsed if response.success?

      message = parsed.dig("error", "message") if parsed.is_a?(Hash)
      error_class = if invalid_page_token_response?(endpoint, response.status, params)
        InvalidPageTokenError
      else
        GoogleApiError
      end
      raise error_class, message.presence || "Google API returned HTTP #{response.status}"
    rescue JSON::ParserError
      if invalid_page_token_response?(endpoint, response&.status, params)
        raise InvalidPageTokenError, "Google API rejected the page token"
      end

      raise GoogleApiError, "Google API returned invalid JSON"
    end

    def net_http_download(endpoint, destination, params)
      uri = URI.parse(endpoint)
      uri.query = URI.encode_www_form(params)
      request = Net::HTTP::Get.new(uri)
      request["Authorization"] = "Bearer #{credential.access_token}"
      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = HttpClient::DEFAULT_OPEN_TIMEOUT
      http.read_timeout = FETCH_READ_TIMEOUT_SECONDS

      http.request(request) do |response|
        status = response.code.to_i
        raise GoogleApiError, "Google API returned HTTP #{status}" unless status.between?(200, 299)

        content_length = response["Content-Length"].to_i
        raise_pdf_too_large if content_length > @max_pdf_bytes

        bytes_written = 0
        response.read_body do |chunk|
          bytes_written = write_pdf_chunk(destination, chunk, bytes_written)
        end
      end
    end

    def write_pdf_chunk(destination, chunk, bytes_written)
      new_size = bytes_written + chunk.bytesize
      raise_pdf_too_large if new_size > @max_pdf_bytes

      destination.write(chunk)
      new_size
    end

    def raise_pdf_too_large
      raise PdfTooLargeError, "PDF exceeds the 50 MB indexing limit"
    end

    def invalid_page_token_response?(endpoint, status, params)
      endpoint == CHANGES_LIST_ENDPOINT && params["pageToken"].present? && [ 400, 404, 410 ].include?(status)
    end
  end
end
