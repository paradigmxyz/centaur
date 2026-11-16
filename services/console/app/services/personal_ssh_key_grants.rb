# Grants a user's personal SSH key to the principals that verifiably are that
# user: their Console principal and the Slack DM principal of their single
# linked Slack identity. Matching never uses email, display names, labels, or
# channel membership, and never reaches shared principals such as channels or
# workflows; giving one of those a key is an explicit admin grant.
#
# Runs when the key is created and whenever a principal or Slack identity that
# could match appears, so a principal created after the key still receives it.
# Grants are only ever added here. Removing one is an explicit revoke, and
# deleting the key removes all of its grants.
class PersonalSshKeyGrants
  CONSOLE_USER_KIND = "console_user".freeze
  SLACK_DM_KIND = "slack_dm".freeze

  def apply_for_user(user)
    key = personal_key(user)
    return 0 unless key && user.active?

    owned_principals(user).to_a.count { |principal| grant(principal, key) }
  end

  def apply_for_principal(principal)
    owner = owner_of(principal)
    key = personal_key(owner)
    return false unless key && owner.active?

    grant(principal, key)
  end

  private

  # Read fresh rather than through user.personal_ssh_key: commit callbacks can
  # run before a has_one association caches a just-created key.
  def personal_key(user)
    user && SshKeySecret.find_by(owner_id: user.id)
  end

  def owned_principals(user)
    scopes = [ Principal.where(kind: CONSOLE_USER_KIND, console_user_id: user.id) ]
    subject, team_id = slack_identity(user)
    if subject
      scopes << Principal.where(kind: SLACK_DM_KIND, slack_user_id: subject, slack_team_id: team_id)
    end
    Principal.combine_scopes(scopes)
  end

  def owner_of(principal)
    case principal.kind
    when CONSOLE_USER_KIND
      principal.console_user
    when SLACK_DM_KIND
      return if principal.slack_user_id.blank? || principal.slack_team_id.blank?

      user = UserIdentity.slack.find_by(subject: principal.slack_user_id, team_id: principal.slack_team_id)&.user
      user if user && slack_identity(user) == [ principal.slack_user_id, principal.slack_team_id ]
    end
  end

  # The user's one unambiguous Slack identity, verified by Slack SSO.
  def slack_identity(user)
    UserIdentity.unambiguous_slack_identity(user.user_identities.slack.order(:id))
  end

  def grant(principal, key)
    return false if principal.grants.exists?(ssh_key_secret: key)

    principal.grants.create!(ssh_key_secret: key, created_by: key.owner)
    true
  rescue ActiveRecord::RecordNotUnique
    false
  end
end
