require "json"
require "time"
require "active_support/tagged_logging"

# Formats each log line as a single-line JSON object. Includes the tagged
# logging formatter so `config.log_tags` (e.g. request ids) keeps working;
# tags are emitted as a "tags" array instead of being prepended to the
# message text.
class JsonLogFormatter < ::Logger::Formatter
  include ActiveSupport::TaggedLogging::Formatter

  def call(severity, time, progname, message)
    entry = {
      time: time.getutc.iso8601(3),
      level: severity
    }
    # Structured events (e.g. lograge with the Raw formatter) arrive as a
    # hash; merge their fields into the entry instead of stringifying them.
    if message.is_a?(::Hash)
      entry.merge!(message)
    else
      entry[:message] = message_text(message)
    end
    entry[:progname] = progname if progname
    tags = current_tags
    entry[:tags] = tags.dup unless tags.empty?
    JSON.generate(entry) << "\n"
  end

  private
    def message_text(message)
      case message
      when ::String
        message
      when ::Exception
        "#{message.message} (#{message.class})\n#{(message.backtrace || []).join("\n")}"
      else
        message.inspect
      end
    end
end
