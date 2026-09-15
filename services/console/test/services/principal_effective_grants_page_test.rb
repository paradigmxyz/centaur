require "test_helper"

class PrincipalEffectiveGrantsPageTest < ActiveSupport::TestCase
  test "counts only enabled effective secrets" do
    principal = principals(:acme_channel)
    disabled = static_secrets(:github_token_inject)
    disabled.update_attribute(:enabled, false)
    relations = {
      "static" => principal.granted_static_secrets,
      "gcp_auth" => principal.granted_gcp_auth_secrets,
      "gcp_id_token" => principal.granted_gcp_id_token_secrets,
      "aws_auth" => principal.granted_aws_auth_secrets,
      "oauth_token" => principal.granted_oauth_token_secrets,
      "pg_dsn" => principal.granted_pg_dsn_secrets,
      "hmac" => principal.granted_hmac_secrets
    }

    result = PrincipalEffectiveGrantsPage.new(
      principal: principal,
      relations: relations,
      page: 1,
      per_page: 50
    ).call

    expected_count = principal.effective_grants
      .includes(*Grant::GRANTABLE_ASSOCIATIONS)
      .filter_map(&:grantable)
      .select(&:enabled?)
      .uniq
      .size
    assert_equal expected_count, result.total_count
    assert_not_includes result.records_by_kind.fetch("static"), disabled
  end
end
