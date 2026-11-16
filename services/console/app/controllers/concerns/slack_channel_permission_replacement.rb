# Console API editing of a principal's or role's Slack channel permissions as
# a whole set (the bearer API upserts one channel at a time through
# SlackChannelPermissionApi). Include next to SlackChannelPermissionApi; the
# controller supplies slack_channel_permission_owner.
module SlackChannelPermissionReplacement
  extend ActiveSupport::Concern

  # PUT /api/v1/console/{principals,roles}/:id/slack_channel_permissions
  #
  # Replaces the owner's Slack channel permissions with data.slack_channel_permissions.
  # Principal DM rows (channel IDs starting with "D") are API-managed: they are
  # kept as stored and any submitted D rows are ignored. An unchanged
  # submission writes nothing (no sync-config bump).
  def replace_slack_channel_permissions
    owner = slack_channel_permission_owner
    rows = slack_channel_permission_params.map(&:to_h)
    if owner.is_a?(Principal)
      rows = rows.reject { |row| direct_message_channel?(row["channel_id"]) }
      rows.concat(owner.slack_channel_permissions.select { |p| direct_message_channel?(p.channel_id) }
                       .map(&:as_permission_json))
    end
    unless owner.slack_channel_permissions_payload == SlackChannelPermission.permission_rows_payload(rows)
      SlackChannelPermission.replace_for!(owner, rows)
    end
    render json: { data: owner.reload.slack_channel_permissions_payload }
  rescue ActiveRecord::RecordInvalid => e
    render_validation_error(e.record)
  end

  # DELETE /api/v1/console/{principals,roles}/:id/slack_channel_permissions/:channel_id
  def destroy_slack_channel_permission
    owner = slack_channel_permission_owner
    owner.slack_channel_permissions.find_by!(channel_id: params[:channel_id].to_s.strip.upcase).destroy!
    head :no_content
  end

  private

  def direct_message_channel?(channel_id)
    channel_id.to_s.strip.upcase.start_with?("D")
  end
end
