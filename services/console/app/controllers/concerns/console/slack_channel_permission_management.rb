module Console
  module SlackChannelPermissionManagement
    extend ActiveSupport::Concern

    private

    def load_slack_channel_permission_form(owner)
      @slack_channel_catalog = SlackChannelCatalogProvider.fetch
      @slack_channel_names = @slack_channel_catalog.channels.to_h { |channel| [ channel.id, channel.name ] }
      @slack_channel_permissions = owner.slack_channel_permissions.ordered
      @slack_channel_options = @slack_channel_catalog.channels.map do |channel|
        label = "##{channel.name} (#{channel.id}) #{channel.private ? "Private" : "Public"}"
        [ label, channel.id ]
      end
    end

    def update_slack_channel_permissions_from_form(owner, path, preserve_api_managed_direct_messages: false)
      rows = slack_channel_permission_replacement_rows(
        owner,
        preserve_api_managed_direct_messages: preserve_api_managed_direct_messages
      )
      if slack_channel_permission_rows_unchanged?(owner, rows)
        return redirect_to path, notice: "Updated Slack channel permissions."
      end

      SlackChannelPermission.replace_for!(owner, rows)
      redirect_to path, notice: "Updated Slack channel permissions."
    rescue ActiveRecord::RecordInvalid => e
      redirect_to path, alert: e.record.errors.full_messages.to_sentence
    rescue ActiveRecord::ReadonlyAttributeError
      redirect_to path, alert: "Slack channels cannot be changed after creation."
    rescue ActiveRecord::RecordNotUnique
      redirect_to path, alert: "Each Slack channel can only be selected once."
    end

    def slack_channel_permission_replacement_rows(owner, preserve_api_managed_direct_messages: false)
      rows = slack_channel_permission_form_rows(owner)
      rows.concat(api_managed_direct_message_rows(owner)) if preserve_api_managed_direct_messages
      rows
    end

    def slack_channel_permission_rows_unchanged?(owner, rows)
      owner.slack_channel_permissions_payload == SlackChannelPermission.permission_rows_payload(rows)
    end

    def slack_channel_permission_form_rows(owner)
      permitted = params.require(owner.model_name.param_key).permit(
        slack_channel_permissions_attributes: [
          :id,
          :channel_id,
          :_destroy,
          *SlackChannelPermission::PERMISSION_ATTRIBUTES
        ]
      )
      rows = permitted.fetch(:slack_channel_permissions_attributes, {}).values
      existing = owner.slack_channel_permissions.index_by { |permission| permission.id.to_s }

      rows.filter_map do |row|
        attrs = row.to_h.with_indifferent_access
        next if ActiveModel::Type::Boolean.new.cast(attrs[:_destroy])

        channel_id = slack_channel_id_for_form_row(attrs, existing)
        next if channel_id.blank?

        { channel_id: channel_id }.merge(
          SlackChannelPermission::PERMISSION_ATTRIBUTES.to_h do |permission|
            [ permission, ActiveModel::Type::Boolean.new.cast(attrs[permission]) ]
          end
        )
      end
    end

    def slack_channel_id_for_form_row(attrs, existing)
      return attrs[:channel_id] if attrs[:id].blank?

      permission = existing.fetch(attrs[:id].to_s) { raise ActiveRecord::RecordNotFound }
      submitted = attrs[:channel_id].to_s.strip.upcase
      raise ActiveRecord::ReadonlyAttributeError if submitted.present? && submitted != permission.channel_id

      permission.channel_id
    end

    def api_managed_direct_message_rows(owner)
      owner.slack_channel_permissions
           .select { |permission| permission.channel_id.to_s.start_with?("D") }
           .map(&:as_permission_json)
    end
  end
end
