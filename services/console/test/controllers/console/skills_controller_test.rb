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

  test "shows editor identities to viewers of a shared skill" do
    skill = skills(:admin_shared)
    editor = users(:globex_admin)
    skill.editors << editor

    get console_skill_url(skill.oid)

    assert_response :ok
    assert_select "#skill-editors-heading", text: "Editors"
    assert_match editor.email, response.body
  end

  test "shows owned and editable skills on the my skills tab" do
    skills(:other_private).editors << @user
    get mine_console_skills_url

    assert_response :ok
    assert_match "My Skills", response.body
    assert_match skills(:member_private).name, response.body
    assert_match skills(:other_private).name, response.body
    assert_no_match skills(:admin_shared).name, response.body
  end

  test "creates edits and immediately shares a skill" do
    assert_difference("Skill.count", 1) do
      post console_skills_url, params: {
        skill: {
          name: "release-helper",
          description: "Help with release readiness.",
          content: "# Release\n\nFollow the release checklist.",
          editor_oids: [ users(:globex_admin).oid ]
        }
      }
    end
    skill = @user.skills.find_by!(name: "release-helper")
    assert_redirected_to console_skill_path(skill.oid)
    assert_equal "# Release\n\nFollow the release checklist.", skill.content
    assert_equal [ users(:globex_admin) ], skill.editors.to_a
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
    assert_select "input[role='combobox'][data-user-typeahead-target='input']"
    assert_select "input[name='skill[editor_oids][]']"
    assert_match "Their names and emails are visible to anyone who can view the skill.", response.body
  end

  test "owner adds and removes editors" do
    skill = skills(:member_private)
    editor = users(:globex_admin)

    assert_difference("SkillEditor.count", 1) do
      patch console_skill_url(skill.oid), params: {
        skill: {
          name: skill.name,
          description: skill.description,
          content: skill.content,
          lock_version: skill.lock_version,
          editor_oids: [ editor.oid ]
        }
      }
    end
    assert_redirected_to console_skill_path(skill.oid)
    assert_equal [ editor ], skill.reload.editors.to_a

    get edit_console_skill_url(skill.oid)
    assert_response :ok
    assert_select "input[name='skill[editor_oids][]'][value='#{editor.oid}']"

    assert_difference("SkillEditor.count", -1) do
      patch console_skill_url(skill.oid), params: {
        skill: {
          name: skill.name,
          description: skill.description,
          content: skill.content,
          lock_version: skill.lock_version,
          editor_oids: [ "" ]
        }
      }
    end
    assert_empty skill.reload.editors
  end

  test "owner cannot add an unavailable editor" do
    skill = skills(:member_private)

    patch console_skill_url(skill.oid), params: {
      skill: {
        name: skill.name,
        description: skill.description,
        content: skill.content,
        lock_version: skill.lock_version,
        editor_oids: [ users(:disabled_user).oid ]
      }
    }

    assert_response :unprocessable_entity
    assert_match "Editors include an unavailable user", response.body
    assert_empty skill.reload.editors
  end

  test "editor updates a private skill but cannot manage it" do
    skill = skills(:other_private)
    skill.editors << @user

    get console_skill_url(skill.oid)
    assert_response :ok
    assert_select "nav[aria-label='Breadcrumb'] a[href='#{mine_console_skills_path}']", text: /Back to Skills/
    assert_select "a[href='#{edit_console_skill_path(skill.oid)}']", text: "Edit"
    assert_select "button[role='switch']", count: 0
    assert_select "form[action='#{console_skill_path(skill.oid)}'] button", text: "Archive", count: 0

    patch console_skill_url(skill.oid), params: {
      skill: {
        name: skill.name,
        description: "Updated by an editor.",
        content: skill.content,
        lock_version: skill.lock_version,
        editor_oids: [ users(:globex_admin).oid ]
      }
    }
    assert_redirected_to console_skill_path(skill.oid)
    assert_equal "Updated by an editor.", skill.reload.description
    assert_equal [ @user ], skill.editors.to_a

    delete console_skill_url(skill.oid)
    assert_response :not_found
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
