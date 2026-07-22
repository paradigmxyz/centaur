module Console
  # View helpers for the principal detail screen.
  module PrincipalsHelper
    def slack_channel_options_for_permission(permission, channel_options)
      current_id = permission.channel_id.to_s
      current_label = if permission.channel_name.present?
        "##{permission.channel_name} (#{current_id})"
      else
        current_id
      end

      options = channel_options.dup
      if current_id.present? && !options.any? { |_label, value| value == current_id }
        options.unshift([ current_label, current_id ])
      end
      options
    end
  end
end
