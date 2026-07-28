module SlackChannelPermissionOwner
  extend ActiveSupport::Concern

  included do
    accepts_nested_attributes_for :slack_channel_permissions,
                                  allow_destroy: true,
                                  reject_if: :reject_slack_channel_permission_attributes?
  end

  def slack_channel_permissions_payload
    permissions = if association(:slack_channel_permissions).loaded?
      slack_channel_permissions.sort_by { |permission| [ permission.channel_id, permission.id ] }
    else
      slack_channel_permissions.ordered
    end
    permissions.map(&:as_permission_json)
  end

  private

  def reject_slack_channel_permission_attributes?(attributes)
    attributes["id"].blank? && attributes["channel_id"].blank?
  end
end
