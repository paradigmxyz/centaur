module SlackChannelPermissionApi
  extend ActiveSupport::Concern

  InvalidSlackChannelPermissions = Class.new(StandardError)

  included do
    rescue_from InvalidSlackChannelPermissions, with: :render_slack_channel_permissions_error
  end

  def upsert_slack_channel_permission
    owner = slack_channel_permission_owner
    attrs = upsert_slack_channel_permission_params
    attrs[:channel_id] = attrs[:channel_id].to_s.strip.upcase
    permission, was_new = save_slack_channel_permission_with_race_retry!(owner, attrs)

    render status: (was_new ? :created : :ok), json: { data: permission.as_permission_json }
  rescue ActiveRecord::RecordInvalid => e
    render_validation_error(e.record)
  end

  # PUT /api/v1/{principals,roles}/:id/slack_channel_permissions
  #
  # Replaces the owner's Slack channel permissions with data.slack_channel_permissions.
  # Principal DM rows (channel IDs starting with "D") are API-managed: they are
  # kept as stored and any submitted D rows are ignored. An unchanged
  # submission writes nothing (no sync-config bump).
  def replace_slack_channel_permissions
    owner = slack_channel_permission_owner
    rows = slack_channel_permission_params.map(&:to_h)
    if preserve_direct_message_permissions?(owner)
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

  # DELETE /api/v1/{principals,roles}/:id/slack_channel_permissions/:channel_id
  def destroy_slack_channel_permission
    owner = slack_channel_permission_owner
    owner.slack_channel_permissions.find_by!(channel_id: params[:channel_id].to_s.strip.upcase).destroy!
    head :no_content
  end

  private

  def preserve_direct_message_permissions?(owner)
    owner.is_a?(Principal)
  end

  def direct_message_channel?(channel_id)
    channel_id.to_s.strip.upcase.start_with?("D")
  end

  def save_slack_channel_permission_with_race_retry!(owner, attrs)
    save_slack_channel_permission!(owner, attrs)
  rescue ActiveRecord::RecordNotUnique
    permission = owner.slack_channel_permissions.find_by!(channel_id: attrs[:channel_id])
    permission.assign_attributes(attrs.except(:channel_id))
    permission.save!
    [ permission, false ]
  end

  def save_slack_channel_permission!(owner, attrs)
    permission = owner.slack_channel_permissions.find_or_initialize_by(channel_id: attrs[:channel_id])
    was_new = permission.new_record?
    permission.assign_attributes(SlackChannelPermission::DEFAULT_ENABLED_ATTRIBUTES) if was_new
    permission.assign_attributes(was_new ? attrs : attrs.except(:channel_id))
    permission.save!
    [ permission, was_new ]
  end

  def replace_slack_channel_permissions!(owner)
    SlackChannelPermission.replace_for!(owner, slack_channel_permission_params)
  end

  def slack_channel_permission_params
    raw = data_params[:slack_channel_permissions]
    unless raw.nil? || raw.is_a?(Array)
      raise InvalidSlackChannelPermissions, "slack_channel_permissions must be an array"
    end

    rows = data_params.permit(
      slack_channel_permissions: [
        :channel_id,
        *SlackChannelPermission::PERMISSION_ATTRIBUTES
      ]
    ).fetch(:slack_channel_permissions, [])

    if raw.present? && rows.length != raw.length
      raise InvalidSlackChannelPermissions, "slack_channel_permissions rows must be objects"
    end

    rows
  end

  def upsert_slack_channel_permission_params
    data_params.permit(
      :channel_id,
      *SlackChannelPermission::PERMISSION_ATTRIBUTES
    )
  end

  def render_slack_channel_permissions_error(error)
    render_error(status: :unprocessable_entity, message: error.message)
  end
end
