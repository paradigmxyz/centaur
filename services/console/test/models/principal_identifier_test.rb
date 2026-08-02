require "test_helper"

class PrincipalIdentifierTest < ActiveSupport::TestCase
  test "normalizes the resolver key and copies the principal namespace" do
    identifier = principals(:acme_user_alice).principal_identifiers.create!(
      namespace: "wrong",
      scheme: "  slack_user  ",
      issuer: "  T123  ",
      subject: "  U123  "
    )

    assert_equal "acme", identifier.namespace
    assert_equal "slack_user", identifier.scheme
    assert_equal "T123", identifier.issuer
    assert_equal "U123", identifier.subject
  end

  test "allows one resolver key to point at separate principals during compatibility" do
    first = principals(:acme_user_alice).principal_identifiers.create!(
      scheme: "slack_user", issuer: "T123", subject: "U123"
    )
    second = principals(:acme_user_bob).principal_identifiers.create!(
      scheme: "slack_user", issuer: "T123", subject: "U123"
    )

    assert_equal first.namespace, second.namespace
    assert_not_equal first.principal_id, second.principal_id
  end

  test "rejects duplicate resolver keys on one principal" do
    principal = principals(:acme_user_alice)
    principal.principal_identifiers.create!(scheme: "slack_user", issuer: "T123", subject: "U123")
    duplicate = principal.principal_identifiers.build(
      scheme: "slack_user", issuer: "T123", subject: "U123"
    )

    assert_not duplicate.valid?
    assert_includes duplicate.errors[:subject], "has already been taken"
  end
end
