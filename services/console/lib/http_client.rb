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
    @http = http
    @open_timeout = open_timeout
    @read_timeout = read_timeout
    @write_timeout = write_timeout
    @max_body_bytes = max_body_bytes
  end

  def get(url, params: {}, headers: {}, timeout: nil, open_timeout: nil, read_timeout: nil,
          write_timeout: nil)
    request(
      method: :get,
      url: url,
      params: params,
      headers: headers,
      timeout: timeout,
      open_timeout: open_timeout,
      read_timeout: read_timeout,
      write_timeout: write_timeout
    )
  end

  def post(url, params: {}, json: nil, form: nil, multipart: false, headers: {}, timeout: nil,
           open_timeout: nil, read_timeout: nil, write_timeout: nil)
    request(
      method: :post,
      url: url,
      params: params,
      json: json,
      form: form,
      multipart: multipart,
      headers: headers,
      timeout: timeout,
      open_timeout: open_timeout,
      read_timeout: read_timeout,
      write_timeout: write_timeout
    )
  end

  def delete(url, params: {}, headers: {}, timeout: nil, open_timeout: nil, read_timeout: nil,
             write_timeout: nil)
    request(
      method: :delete,
      url: url,
      params: params,
      headers: headers,
      timeout: timeout,
      open_timeout: open_timeout,
      read_timeout: read_timeout,
      write_timeout: write_timeout
    )
  end

  def request(method:, url:, params: {}, json: nil, form: nil, multipart: false, headers: {},
              timeout: nil, open_timeout: nil, read_timeout: nil, write_timeout: nil)
    uri = build_uri(url, params)
    request_headers = default_headers.merge(headers)
    apply_content_type(request_headers, json: json, form: form, multipart: multipart)
    body = request_body(json: json, form: form)

    raw_response = if @http
      @http.call(
        method: method,
        url: uri.to_s,
        body: body,
        headers: request_headers,
        timeout: timeout || read_timeout || @read_timeout || @open_timeout || @write_timeout
      )
    else
      net_http_request(
        method: method,
        uri: uri,
        headers: request_headers,
        json: json,
        form: form,
        multipart: multipart,
        body: body,
        timeout: timeout,
        open_timeout: open_timeout,
        read_timeout: read_timeout,
        write_timeout: write_timeout
      )
    end

    normalize_response(raw_response)
  end

  private

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

  def net_http_request(method:, uri:, headers:, json:, form:, multipart:, body:, timeout:,
                       open_timeout:, read_timeout:, write_timeout:)
    request = REQUEST_CLASSES.fetch(method).new(uri)
    headers.each { |key, value| request[key] = value }
    apply_body(request, json: json, form: form, multipart: multipart, body: body)

    http = Net::HTTP.new(uri.host, uri.port)
    http.use_ssl = uri.scheme == "https"
    http.open_timeout = open_timeout || timeout || @open_timeout
    http.read_timeout = read_timeout || timeout || @read_timeout
    resolved_write_timeout = write_timeout || timeout || @write_timeout
    http.write_timeout = resolved_write_timeout if resolved_write_timeout

    http.request(request)
  end

  def apply_body(request, json:, form:, multipart:, body:)
    if !json.nil?
      request.body = body
    elsif form
      if multipart
        request.set_form(form.to_a, "multipart/form-data")
      else
        request.set_form_data(form)
      end
    end
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

  def normalize_response(response)
    return response if response.is_a?(Response)

    headers = {}
    if response.respond_to?(:to_hash)
      response.to_hash.each do |key, value|
        headers[key.to_s.downcase] = Array(value).join(", ")
      end
    end
    body = response.body.to_s
    body = body.byteslice(0, @max_body_bytes) if @max_body_bytes
    Response.new(status: response.code.to_i, body: body, headers: headers)
  end

  def default_headers
    { "Accept" => "application/json" }
  end
end
