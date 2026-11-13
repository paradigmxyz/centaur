# Turns the console scheduled-task form fields into model attributes.
#
#   schedule_preset  manual | daily | weekdays | mondays | fridays | custom
#                    (custom uses custom_days ["1".."6","0"] + custom_time "HH:MM";
#                    "cron" uses cron_expression verbatim)
#   delivery_mode    dm (the author's Slack DM) | channel (delivery_channel)
#
# Only fields present in the input are changed, so a partial update keeps the
# stored schedule and destination. The model validates the result, including
# that the destination is available to the task's author.
class ScheduledTaskConsoleAttributes
  INVALID_CUSTOM_SCHEDULE = "invalid custom schedule".freeze

  def self.call(task, input)
    new(task, input).attributes
  end

  def initialize(task, input)
    @task = task
    @input = input.to_h.with_indifferent_access
  end

  def attributes
    attributes = input.slice(:name, :prompt, :enabled)
    attributes[:cron_expression] = cron_expression if input.key?(:schedule_preset)
    attributes[:delivery_channel] = delivery_channel if input.key?(:delivery_mode) || input.key?(:delivery_channel)
    attributes
  end

  private

  attr_reader :task, :input

  def cron_expression
    preset = input[:schedule_preset].to_s
    cron = ScheduledTask.cron_for(preset, input[:cron_expression],
                                  custom_days: Array(input[:custom_days]), custom_time: input[:custom_time])
    preset == "custom" && cron.blank? ? INVALID_CUSTOM_SCHEDULE : cron
  end

  def delivery_channel
    case input[:delivery_mode].presence || "channel"
    when "dm" then task.author && SlackDeliveryPolicy.new(task.author).direct_message_user_id
    when "channel" then input[:delivery_channel]
    end
  end
end
