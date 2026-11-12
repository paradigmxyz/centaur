# Pins the number of SQL queries a request issues, so an endpoint cannot
# regress into one query (or query-cache hit) per row.
#
#   assert_queries_do_not_grow(-> { get api_v1_console_scheduled_tasks_path }) do
#     3.times { create_task }
#   end
#
# Counts every statement except schema lookups and transaction control,
# including query-cache hits: a per-row lookup served from the cache still
# costs a round trip through Active Record and grows with the page.
module QueryCountHelper
  IGNORED_STATEMENT = /\A\s*(BEGIN|COMMIT|ROLLBACK|SAVEPOINT|RELEASE)/i

  # The SQL statements +block+ issues.
  def capture_queries(&block)
    queries = []
    callback = lambda do |*, payload|
      next if payload[:name] == "SCHEMA" || payload[:sql].match?(IGNORED_STATEMENT)

      queries << payload[:sql]
    end
    ActiveSupport::Notifications.subscribed(callback, "sql.active_record", &block)
    queries
  end

  # Runs +request+, then +grow+ (which adds rows the request renders), then
  # +request+ again, and asserts the second run issued no more queries than
  # the first.
  def assert_queries_do_not_grow(request, &grow)
    before = capture_queries(&request)
    grow.call
    after = capture_queries(&request)
    assert_operator after.size, :<=, before.size,
                    "query count grew from #{before.size} to #{after.size}:\n#{(after - before).uniq.join("\n")}"
  end

  # The number of Active Record objects +block+ instantiates, by class name.
  def capture_instantiations(&block)
    counts = Hash.new(0)
    callback = ->(*, payload) { counts[payload[:class_name]] += payload[:record_count] }
    ActiveSupport::Notifications.subscribed(callback, "instantiation.active_record", &block)
    counts
  end
end
