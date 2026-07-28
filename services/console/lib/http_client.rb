require "json"
require "net/http"
require "uri"

class HttpClient
  DEFAULT_OPEN_TIMEOUT = 5
  DEFAULT_READ_TIMEOUT = 5

  REQUEST_CLASSES = {
    delete: Net::HTTP::Delete,
    get: Net::HTTP::Get,
    post: Net::HTTP::Post
  }.freeze

  Response = Struct.new(:status, :body, :headers, keyword_init: true) do
    def [](name)
      (headers || {}).fetch(name.to_s.downcase, nil)
    end

    def success?
      status.between?(200, 299)
    end

    def json
      @json ||= HttpClient.decode_json_body(body)
    end
  end

  def self.decode_json_body(body)
    text = body.to_s
    return {} if text.blank?

    JSON.parse(text)
  end

  def initialize(http: nil, open_timeout: DEFAULT_OPEN_TIMEOUT, read_timeout: DEFAULT_READ_TIMEOUT,
                 write_timeout: nil, max_body_bytes: nil)
    @http = if http
      InjectedTransport.new(http, timeout: read_timeout || open_timeout || write_timeout)
    else
      NetHttpTransport.new(
        open_timeout: open_timeout,
        read_timeout: read_timeout,
        write_timeout: write_timeout,
        max_body_bytes: max_body_bytes
      )
    end
  end

  def get(url, params: {}, headers: {})
    request(
      method: :get,
      url: url,
      params: params,
      headers: headers
    )
  end

  def post(url, params: {}, json: nil, form: nil, multipart: false, headers: {})
    request(
      method: :post,
      url: url,
      params: params,
      json: json,
      form: form,
      multipart: multipart,
      headers: headers
    )
  end

  def delete(url, params: {}, headers: {})
    request(
      method: :delete,
      url: url,
      params: params,
      headers: headers
    )
  end

  def request(method:, url:, params: {}, json: nil, form: nil, multipart: false, headers: {})
    uri = build_uri(url, params)
    request_headers = default_headers.merge(headers)
    apply_content_type(request_headers, json: json, form: form, multipart: multipart)
    body = request_body(json: json, form: form)

    @http.call(
      method: method,
      url: uri.to_s,
      body: body,
      form: form,
      multipart: multipart,
      headers: request_headers
    )
  end

  private

  class InjectedTransport
    def initialize(http, timeout:)
      @http = http
      @timeout = timeout
    end

    def call(method:, url:, body:, headers:, form:, multipart:)
      @http.call(
        method: method,
        url: url,
        body: body,
        headers: headers,
        timeout: @timeout
      )
    end
  end

  class NetHttpTransport
    def initialize(open_timeout:, read_timeout:, write_timeout:, max_body_bytes:)
      @timeouts = {
        open: open_timeout,
        read: read_timeout,
        write: write_timeout
      }
      @max_body_bytes = max_body_bytes
    end

    def call(method:, url:, body:, headers:, form:, multipart:)
      uri = URI.parse(url)
      request = REQUEST_CLASSES.fetch(method).new(uri)
      apply_body(request, body: body, form: form, multipart: multipart)
      headers.each { |key, value| request[key] = value }

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      apply_timeouts(http)

      response = http.request(request)
      Response.new(
        status: response.code.to_i,
        body: response_body(response),
        headers: response_headers(response)
      )
    end

    private

    def apply_timeouts(http)
      http.open_timeout = @timeouts.fetch(:open)
      http.read_timeout = @timeouts.fetch(:read)

      write_timeout = @timeouts.fetch(:write)
      http.write_timeout = write_timeout if write_timeout
    end

    def response_body(response)
      body = response.body.to_s
      @max_body_bytes ? body.byteslice(0, @max_body_bytes) : body
    end

    def response_headers(response)
      return {} unless response.respond_to?(:to_hash)

      response.to_hash.to_h do |key, value|
        [ key.to_s.downcase, Array(value).join(", ") ]
      end
    end

    def apply_body(request, body:, form:, multipart:)
      if form
        if multipart
          request.set_form(form.to_a, "multipart/form-data")
        else
          request.set_form_data(form)
        end
      elsif body
        request.body = body
      end
    end
  end

  def build_uri(url, params)
    uri = URI.parse(url)
    compact_params = params.compact
    return uri if compact_params.empty?

    existing = uri.query.present? ? URI.decode_www_form(uri.query) : []
    uri.query = URI.encode_www_form(existing + compact_params.map { |key, value| [ key, value.to_s ] })
    uri
  end

  def request_body(json:, form:)
    return json.to_json unless json.nil?
    return URI.encode_www_form(form) if form

    nil
  end

  def apply_content_type(headers, json:, form:, multipart:)
    return if header?(headers, "Content-Type")

    if !json.nil?
      headers["Content-Type"] = "application/json"
    elsif form && !multipart
      headers["Content-Type"] = "application/x-www-form-urlencoded"
    end
  end

  def header?(headers, name)
    headers.any? { |key, _value| key.to_s.casecmp?(name) }
  end

  def default_headers
    { "Accept" => "application/json" }
  end
end
