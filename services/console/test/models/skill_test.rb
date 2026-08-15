require "test_helper"

class SkillTest < ActiveSupport::TestCase
  test "defaults new skills to shared" do
    skill = users(:member_user).skills.create!(attributes(name: "public-by-default"))

    assert skill.shared?
    assert_not_nil skill.shared_at
  end

  test "validates ordinary attributes and serializes a SKILL.md document" do
    skill = users(:member_user).skills.create!(
      name: " release-check ",
      description: " Check release readiness. ",
      content: "# Steps\r\n\r\nRun the checks.\r\n"
    )

    assert_equal "release-check", skill.name
    assert_equal "Check release readiness.", skill.description
    assert_equal "# Steps\n\nRun the checks.", skill.content
    assert_equal <<~MARKDOWN, skill.skill_document
      ---
      name: release-check
      description: Check release readiness.
      ---

      # Steps

      Run the checks.
    MARKDOWN
    assert_match(/\A[0-9a-f]{64}\z/, skill.checksum)
  end

  test "serializes descriptions as safe YAML frontmatter" do
    skill = users(:member_user).skills.create!(
      name: "safe-frontmatter",
      description: "Investigate incidents: use logs first.",
      content: "# Instructions\n\nInspect logs."
    )
    frontmatter = skill.skill_document.delete_prefix("---\n").split("\n---\n", 2).first

    assert_equal(
      { "name" => "safe-frontmatter", "description" => "Investigate incidents: use logs first." },
      YAML.safe_load(frontmatter)
    )
  end

  test "validates name description and instructions" do
    skill = users(:member_user).skills.new(name: "Invalid Name", description: "", content: "")

    assert_not skill.valid?
    assert_includes skill.errors[:name], "is invalid"
    assert_includes skill.errors[:description], "can't be blank"
    assert_includes skill.errors[:content], "can't be blank"
  end

  test "reserves the OID namespace for skill IDs" do
    skill = users(:member_user).skills.new(attributes(name: "skl_example"))

    assert_not skill.valid?
    assert_includes skill.errors[:name], "is invalid"
  end

  test "reserves names used by sandbox skill routes" do
    skill = users(:member_user).skills.new(attributes(name: "search"))

    assert_not skill.valid?
    assert_includes skill.errors[:name], "is reserved"
  end

  test "enforces globally unique active names" do
    duplicate = users(:member_user).skills.new(attributes(name: skills(:member_private).name))
    assert_not duplicate.valid?
    assert_includes duplicate.errors[:name], "is already used by another skill"

    other_user = users(:member_user).skills.new(attributes(name: skills(:admin_shared).name))
    assert_not other_user.valid?
    assert_includes other_user.errors[:name], "is already used by another skill"
  end

  test "search ranks matching visible documents" do
    results = Skill.catalog_visible_to(users(:member_user)).search("production incidents")

    assert_includes results, skills(:member_private)
    assert_not_includes results, skills(:other_private)
  end

  test "editors can access private skills without becoming owners" do
    skill = skills(:other_private)
    editor = users(:member_user)
    skill.editors << editor

    assert_includes Skill.editable_by(editor), skill
    assert_includes Skill.catalog_visible_to(editor), skill
    assert skill.editable_by?(editor)
    assert_not_equal editor, skill.user
  end

  test "owner cannot also be assigned as an editor" do
    assignment = skills(:member_private).skill_editors.new(user: users(:member_user))

    assert_not assignment.valid?
    assert_includes assignment.errors[:user], "is already the skill owner"
  end

  test "search boosts names over descriptions and instructions" do
    name_match = users(:member_user).skills.create!(
      attributes(name: "deploy-runbook").merge(description: "Check release readiness.", content: "# Steps\n\nReview the checklist.")
    )
    description_match = users(:member_user).skills.create!(
      attributes(name: "release-runbook").merge(description: "Deploy releases safely.", content: "# Steps\n\nReview the checklist.")
    )
    instructions_match = users(:member_user).skills.create!(
      attributes(name: "operations-runbook").merge(description: "Operate services safely.", content: "# Steps\n\nDeploy releases safely.")
    )

    results = Skill.search("deploy").to_a

    assert_operator results.index(name_match), :<, results.index(description_match)
    assert_operator results.index(description_match), :<, results.index(instructions_match)
  end

  test "sharing and archiving mutate the current document" do
    skill = skills(:member_private)
    skill.share!
    assert skill.shared?
    assert_not_nil skill.shared_at

    skill.archive!
    assert_not_nil skill.archived_at
    assert_equal "private", skill.visibility
    assert_not_includes Skill.active, skill

    replacement = users(:acme_admin).skills.create!(attributes(name: skill.name))
    assert_equal skill.name, replacement.name
  end

  private

  def attributes(name: "release-check")
    {
      name: name,
      description: "Check release readiness.",
      content: "# Steps\n\nRun the checks."
    }
  end
end
