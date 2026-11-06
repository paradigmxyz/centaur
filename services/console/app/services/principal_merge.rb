# Folds +source+ into +target+ when the same user ended up with two
# principals: everything that references the source moves to the target, the
# source is deleted, and its foreign_id (plus any identities it held) becomes
# an identity of the target so callers that derive or stored the old id keep
# reaching the same principal.
# Where both hold the same grant, role, client link, or Slack channel, the
# target's row is kept; Slack channel flags combine so neither side loses
# access it had.
class PrincipalMerge
  def self.call(source:, target:)
    new(source: source, target: target).call
  end

  def initialize(source:, target:)
    raise ArgumentError, "a principal cannot be merged into itself" if source == target

    @source = source
    @target = target
  end

  def call
    Principal.transaction do
      source.lock!
      target.lock!
      identity_ids = source.identities.pluck(:foreign_id) + [ source.foreign_id ].compact

      move_grants
      move_unique_links(PrincipalRole, :role_id)
      move_api_client_links
      move_slack_channel_permissions
      Proxy.where(principal_id: source.id).update_all(principal_id: target.id)
      Proxy.where(requester_principal_id: source.id).update_all(requester_principal_id: target.id)
      McpOauthAuthorizationCode.where(principal_id: source.id).update_all(principal_id: target.id)
      McpOauthRefreshToken.where(principal_id: source.id).update_all(principal_id: target.id)

      labels = source.labels.to_h.merge(target.labels.to_h)
      source.reload.destroy!
      identity_ids.each { |foreign_id| target.identities.find_or_create_by!(foreign_id: foreign_id) }
      target.update!(labels: labels)
      Principal.bump_sync_config_cache_versions(target.id)
      target
    end
  end

  private

  attr_reader :source, :target

  def move_grants
    Grant::GRANTABLE_ASSOCIATIONS.each do |grantable|
      column = :"#{grantable}_id"
      held = Grant.where(principal_id: target.id).where.not(column => nil).select(column)
      Grant.where(principal_id: source.id, column => held).delete_all
    end
    Grant.where(principal_id: source.id).update_all(principal_id: target.id)
  end

  def move_unique_links(model, column)
    held = model.where(principal_id: target.id).select(column)
    model.where(principal_id: source.id, column => held).delete_all
    model.where(principal_id: source.id).update_all(principal_id: target.id)
  end

  # A client's default link stays its default, even when the target already
  # had a non-default link to that client.
  def move_api_client_links
    ApiClientPrincipal.where(principal_id: source.id).find_each do |link|
      existing = ApiClientPrincipal.find_by(api_client_id: link.api_client_id, principal_id: target.id)
      if existing
        link.delete
        existing.update_columns(default: true) if link.default
      else
        ApiClientPrincipal.where(id: link.id).update_all(principal_id: target.id)
      end
    end
  end

  def move_slack_channel_permissions
    held = SlackChannelPermission.where(principal_id: target.id).index_by(&:channel_id)
    SlackChannelPermission.where(principal_id: source.id, channel_id: held.keys).find_each do |permission|
      existing = held.fetch(permission.channel_id)
      flags = SlackChannelPermission::PERMISSION_ATTRIBUTE_NAMES.index_with do |flag|
        existing.public_send(flag) || permission.public_send(flag)
      end
      permission.delete
      existing.update_columns(flags)
    end
    SlackChannelPermission.where(principal_id: source.id).update_all(principal_id: target.id)
  end
end
