ENV["RAILS_ENV"] ||= "test"
# Disable libpq's Kerberos/GSSAPI negotiation. On macOS this loads frameworks
# that segfault when the test runner forks parallel workers.
ENV["PGGSSENCMODE"] ||= "disable"
require_relative "../config/environment"
require "rails/test_help"

module ActiveSupport
  class TestCase
    # Run tests in parallel with specified workers
    parallelize(workers: :number_of_processors)

    # Setup all fixtures in test/fixtures/*.yml for all tests in alphabetical order.
    fixtures :all

    # Add more helper methods to be used by all tests here...
  end
end
