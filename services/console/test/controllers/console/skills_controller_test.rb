require "test_helper"

class Console::SkillsControllerTest < ActionDispatch::IntegrationTest
  setup do
    @user = users(:member_user)
    post login_url, params: { email: @user.email, password: "password123456" }
  end

  test "shows public skills by default" do
    get console_skills_url

    assert_response :ok
    assert_match "Signed in as", response.body
    assert_match "Public Skills", response.body
    assert_match skills(:admin_shared).name, response.body
    assert_no_match skills(:member_private).name, response.body
    assert_no_match skills(:other_private).name, response.body
  end

  test "shows only owned skills on the my skills tab" do
    get mine_console_skills_url

    assert_response :ok
    assert_match "My Skills", response.body
    assert_match skills(:member_private).name, response.body
    assert_no_match skills(:admin_shared).name, response.body
    assert_no_match skills(:other_private).name, response.body
  end

  test "creates edits and immediately shares a skill" do
    assert_difference("Skill.count", 1) do
      post console_skills_url, params: {
        skill: {
          name: "release-helper",
          description: "Help with release readiness.",
          content: "# Release\n\nFollow the release checklist."
        }
      }
    end
    skill = @user.skills.find_by!(name: "release-helper")
    assert_redirected_to console_skill_path(skill.oid)
    assert_equal "# Release\n\nFollow the release checklist.", skill.content
    assert skill.shared?
    assert_not_nil skill.shared_at

    post share_console_skill_url(skill.oid)
    assert_redirected_to console_skill_path(skill.oid)
    assert skill.reload.shared?

    patch console_skill_url(skill.oid), params: {
      skill: {
        name: "release-helper",
        description: "Updated release guidance.",
        content: "# Release\n\nUpdated live.",
        lock_version: skill.lock_version
      }
    }
    assert_redirected_to console_skill_path(skill.oid)
    assert_equal "Updated release guidance.", skill.reload.description
    assert_includes skill.reload.content, "Updated live."
  end

  test "renders separate frontmatter and instruction fields" do
    get new_console_skill_url

    assert_response :ok
    assert_select "input[name='skill[name]']"
    assert_select "input[name='skill[description]']"
    assert_select "textarea[name='skill[content]']"
  end

  test "preserves separate fields when instructions are invalid" do
    post console_skills_url, params: {
      skill: {
        name: "release-helper",
        description: "Help with release readiness.",
        content: ""
      }
    }

    assert_response :unprocessable_entity
    assert_select "input[name='skill[name]'][value='release-helper']"
    assert_select "input[name='skill[description]'][value='Help with release readiness.']"
    assert_select "textarea[name='skill[content]']", text: ""
  end

  test "cannot read another user's private skill" do
    get console_skill_url(skills(:other_private).oid)
    assert_response :not_found
  end

  test "renders the skill instructions without raw document or preview panes" do
    get console_skill_url(skills(:member_private).oid)

    assert_response :ok
    assert_select "nav[aria-label='Breadcrumb'] a[href='#{mine_console_skills_path}']", text: /Back to Skills/
    assert_select "#skill-description-heading", text: "Description"
    assert_select "#skill-instructions-heading", text: "Instructions"
    assert_select ".console-markdown h1", text: "Incident Triage"
    assert_select "button[role='switch'][aria-checked='false']" do
      assert_select "span", text: "Private"
      assert_select "span", text: "Public"
    end
    assert_select "pre", count: 0
    assert_select "h2", text: "Preview", count: 0
  end
end
