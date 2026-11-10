ENV["RAILS_ENV"] ||= "test"
# Disable libpq's Kerberos/GSSAPI negotiation. On macOS this loads frameworks
# that segfault when the test runner forks parallel workers.
ENV["PGGSSENCMODE"] ||= "disable"
require_relative "../config/environment"
require "rails/test_help"
require "minitest/mock"
require "open3"
require "tmpdir"

# Backport rails/rails#58875: Rails 8.1.4 casts an unqualified table name to
# regclass when validating fixture foreign keys, so tables outside the search
# path fail. Remove after upgrading to a Rails release that includes the fix.
ActiveRecord::ConnectionAdapters::PostgreSQLAdapter.prepend(Module.new do
  def check_all_foreign_keys_valid!
    sql = <<~SQL
      do $$
        declare r record;
      BEGIN
      FOR r IN (
        SELECT FORMAT(
          'UPDATE pg_catalog.pg_constraint SET convalidated=false WHERE conname = ''%1$I'' AND connamespace::regnamespace = ''%2$I''::regnamespace AND conrelid::regclass = ''%2$I.%3$I''::regclass; ALTER TABLE %2$I.%3$I VALIDATE CONSTRAINT %1$I;',
          constraint_name,
          table_schema,
          table_name
        ) AS constraint_check
        FROM information_schema.table_constraints WHERE constraint_type = 'FOREIGN KEY'
      )
        LOOP
          EXECUTE (r.constraint_check);
        END LOOP;
      END;
      $$;
    SQL

    transaction(requires_new: true) do
      execute(sql)
    end
  end
end)

module SyntheticSshKey
  def self.pair
    @pair ||= generate
  end

  def self.generate
    Dir.mktmpdir do |dir|
      path = File.join(dir, "synthetic")
      _out, _err, status = Open3.capture3("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", path)
      raise "synthetic key generation failed" unless status.success?

      [ File.read(path), File.read("#{path}.pub").split.first(2).join(" ") ]
    end
  end
end

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

    # A freshly generated, unencrypted Ed25519 key pair as [private, public].
    # Generated per process so no key material lives in fixtures.
    def synthetic_ssh_key
      SyntheticSshKey.pair
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
