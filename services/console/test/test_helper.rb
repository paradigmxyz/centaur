ENV["RAILS_ENV"] ||= "test"
# Disable libpq's Kerberos/GSSAPI negotiation. On macOS this loads frameworks
# that segfault when the test runner forks parallel workers.
ENV["PGGSSENCMODE"] ||= "disable"
require_relative "../config/environment"
require "rails/test_help"
require "minitest/mock"

module ActiveSupport
  class TestCase
    # Run tests in parallel with specified workers
    parallelize(workers: :number_of_processors)

    # Setup all fixtures in test/fixtures/*.yml for all tests in alphabetical order.
    fixtures :all

    def expect_http_call(http = Minitest::Mock.new, status:, body:, headers: nil)
      http.expect(:call, HttpClient::Response.new(status: status, body: body, headers: headers)) do |**request|
        yield request if block_given?
        true
      end
      http
    end

    def with_env(overrides)
      previous = overrides.keys.index_with { |name| ENV[name] }
      overrides.each { |name, value| value.nil? ? ENV.delete(name) : ENV[name] = value }
      yield
    ensure
      previous.each { |name, value| value.nil? ? ENV.delete(name) : ENV[name] = value }
    end
  end
end
