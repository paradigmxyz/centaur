require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does.
    class ConsoleSkillsTest < ActionDispatch::IntegrationTest
      setup do
        @user = users(:member_user)
        sign_in_console(@user)
      end

      def skill_body(skill, **overrides)
        { data: { name: skill.name, description: skill.description, instructions: skill.content,
                  lock_version: skill.lock_version }.merge(overrides) }
      end

      test "skill endpoints are open to active users only" do
        skill = skills(:admin_shared)
        assert_active_user_api :get, api_v1_console_skills_path
        assert_active_user_api :get, api_v1_console_skills_path(tab: "mine")
        assert_active_user_api :get, api_v1_console_skill_path(skill.oid)
        assert_active_user_api :get, editor_candidates_api_v1_console_skills_path
        assert_active_user_api :post, api_v1_console_skills_path,
                               params: { data: { name: "denied-skill", description: "d", instructions: "x" } }
        assert_active_user_api :patch, api_v1_console_skill_path(skill.oid), params: skill_body(skill, description: "denied")
        assert_active_user_api :delete, api_v1_console_skill_path(skill.oid)
        assert_active_user_api :post, share_api_v1_console_skill_path(skill.oid)
        assert_active_user_api :post, unshare_api_v1_console_skill_path(skill.oid)

        assert_not Skill.exists?(name: "denied-skill")
        skill.reload
        assert_equal "Research a customer using approved internal sources.", skill.description
        assert skill.shared?
        assert_nil skill.archived_at
      end

      test "shows public skills by default" do
        get api_v1_console_skills_path

        assert_response :ok
        names = json_body["data"].map { |s| s["name"] }
        assert_includes names, skills(:admin_shared).name
        assert_not_includes names, skills(:member_private).name
        assert_not_includes names, skills(:other_private).name
        row = json_body["data"].find { |s| s["name"] == skills(:admin_shared).name }
        assert_equal "shared", row["visibility"]
        assert_equal users(:acme_admin).email, row.dig("owner", "email")
        assert_equal false, row.dig("viewer", "owner")
        assert_nil row["instructions"], "the list omits skill content"
      end

      test "shows owned and editable skills on the my skills tab" do
        skills(:other_private).editors << @user
        get api_v1_console_skills_path(tab: "mine")

        assert_response :ok
        names = json_body["data"].map { |s| s["name"] }
        assert_includes names, skills(:member_private).name
        assert_includes names, skills(:other_private).name
        assert_not_includes names, skills(:admin_shared).name
        mine = json_body["data"].find { |s| s["name"] == skills(:member_private).name }
        assert_equal "private", mine["visibility"]
        assert mine.dig("viewer", "owner")
      end

      test "index searches within the selected tab and hides archived skills" do
        get api_v1_console_skills_path(q: "customer")
        assert_equal [ skills(:admin_shared).oid ], json_body["data"].map { |s| s["id"] }

        get api_v1_console_skills_path(tab: "mine", q: "customer")
        assert_empty json_body["data"]

        skills(:member_private).archive!
        get api_v1_console_skills_path(tab: "mine")
        assert_empty json_body["data"]
      end

      test "shows editor identities to viewers of a shared skill" do
        skill = skills(:admin_shared)
        editor = users(:globex_admin)
        skill.editors << editor

        get api_v1_console_skill_path(skill.oid)

        assert_response :ok
        assert_equal [ { "id" => editor.oid, "name" => editor.name.presence, "email" => editor.email } ],
                     json_body.dig("data", "editors")
        assert_equal({ "owner" => false, "can_edit" => false, "can_manage" => false, "can_unshare" => false },
                     json_body.dig("data", "viewer"))
      end

      test "cannot read another user's private skill" do
        get api_v1_console_skill_path(skills(:other_private).oid)
        assert_response :not_found
      end

      test "renders the skill instructions as sanitized markdown" do
        skill = skills(:member_private)
        skill.update!(content: "# Incident Triage\n\n<script>alert(1)</script> [docs](javascript:alert(1))")

        get api_v1_console_skill_path(skill.oid)

        assert_response :ok
        data = json_body["data"]
        assert_equal skill.content, data["instructions"]
        html = Nokogiri::HTML5.fragment(data["content_html"])
        assert_equal "Incident Triage", html.at_css("h1").text
        assert_nil html.at_css("script")
        assert_nil html.at_css("a"), "only http(s) links are rendered"
        assert_empty html.css("pre")
        assert_equal "private", data["visibility"]
        assert_equal({ "owner" => true, "can_edit" => true, "can_manage" => true, "can_unshare" => false },
                     data["viewer"])
        assert_equal skill.lock_version, data["lock_version"]
      end

      test "an admin may unshare any shared skill" do
        skill = skills(:admin_shared)
        skill.update!(user: users(:globex_admin))
        sign_in_console(users(:acme_admin))

        get api_v1_console_skill_path(skill.oid)
        assert json_body.dig("data", "viewer", "can_unshare")
        assert_not json_body.dig("data", "viewer", "can_manage")

        post unshare_api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :ok
        assert_equal "private", json_body.dig("data", "visibility")
        assert_not skill.reload.shared?

        delete api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :not_found, "moderation does not extend to archiving"
        assert_nil skill.reload.archived_at
      end

      test "a non-admin cannot unshare someone else's shared skill" do
        post unshare_api_v1_console_skill_path(skills(:admin_shared).oid), headers: csrf_headers
        assert_response :not_found
        assert skills(:admin_shared).reload.shared?
      end

      test "creates edits and immediately shares a skill" do
        assert_difference("Skill.count", 1) do
          post api_v1_console_skills_path, params: {
            data: {
              name: "release-helper",
              description: "Help with release readiness.",
              instructions: "# Release\n\nFollow the release checklist.",
              editor_ids: [ users(:globex_admin).oid ]
            }
          }, headers: csrf_headers, as: :json
        end
        assert_response :created
        skill = @user.skills.find_by!(name: "release-helper")
        assert_equal skill.oid, json_body.dig("data", "id")
        assert_equal "# Release\n\nFollow the release checklist.", skill.content
        assert_equal [ users(:globex_admin) ], skill.editors.to_a
        assert skill.shared?
        assert_not_nil skill.shared_at

        post share_api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :ok
        assert skill.reload.shared?

        patch api_v1_console_skill_path(skill.oid), params: {
          data: {
            name: "release-helper",
            description: "Updated release guidance.",
            instructions: "# Release\n\nUpdated live.",
            lock_version: skill.lock_version
          }
        }, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal "Updated release guidance.", skill.reload.description
        assert_includes skill.reload.content, "Updated live."
        assert_equal [ users(:globex_admin) ], skill.editors.to_a, "omitting editor_ids keeps the editors"
      end

      test "owner shares, unshares, and archives a skill" do
        skill = skills(:member_private)

        post share_api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :ok
        assert_equal "shared", json_body.dig("data", "visibility")
        assert json_body.dig("data", "viewer", "can_unshare")

        post unshare_api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :ok
        assert_equal "private", json_body.dig("data", "visibility")

        delete api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :no_content
        assert_not_nil skill.reload.archived_at
        get api_v1_console_skill_path(skill.oid)
        assert_response :not_found
      end

      test "owner adds and removes editors" do
        skill = skills(:member_private)
        editor = users(:globex_admin)

        assert_difference("SkillEditor.count", 1) do
          patch api_v1_console_skill_path(skill.oid), params: skill_body(skill, editor_ids: [ editor.oid ]),
                                              headers: csrf_headers, as: :json
        end
        assert_response :ok
        assert_equal [ editor ], skill.reload.editors.to_a
        assert_equal [ editor.oid ], json_body.dig("data", "editors").map { |e| e["id"] }

        assert_difference("SkillEditor.count", -1) do
          patch api_v1_console_skill_path(skill.oid), params: skill_body(skill.reload, editor_ids: []),
                                              headers: csrf_headers, as: :json
        end
        assert_response :ok
        assert_empty skill.reload.editors
      end

      test "owner cannot add an unavailable editor" do
        skill = skills(:member_private)

        [ users(:disabled_user).oid, "usr_bogus", @user.oid ].each do |editor_id|
          patch api_v1_console_skill_path(skill.oid), params: skill_body(skill, editor_ids: [ editor_id ]),
                                              headers: csrf_headers, as: :json

          assert_response :unprocessable_entity
          assert_equal [ "include an unavailable user" ], json_body.dig("error", "details", "editors"), editor_id
        end
        assert_empty skill.reload.editors
      end

      test "editor updates a private skill but cannot manage it" do
        skill = skills(:other_private)
        skill.editors << @user

        get api_v1_console_skill_path(skill.oid)
        assert_response :ok
        assert_equal({ "owner" => false, "can_edit" => true, "can_manage" => false, "can_unshare" => false },
                     json_body.dig("data", "viewer"))

        patch api_v1_console_skill_path(skill.oid), params: skill_body(skill, description: "Updated by an editor.",
                                                                      editor_ids: [ users(:globex_admin).oid ]),
                                            headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal "Updated by an editor.", skill.reload.description
        assert_equal [ @user ], skill.editors.to_a

        delete api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :not_found
        post share_api_v1_console_skill_path(skill.oid), headers: csrf_headers
        assert_response :not_found
        assert_not skill.reload.shared?
        assert_nil skill.archived_at
      end

      test "a stranger cannot update another user's private skill" do
        skill = skills(:other_private)
        patch api_v1_console_skill_path(skill.oid), params: skill_body(skill, description: "hijacked"),
                                            headers: csrf_headers, as: :json
        assert_response :not_found
        assert_equal "Private finance guidance for the owner.", skill.reload.description
      end

      test "preserves separate fields when instructions are invalid" do
        assert_no_difference("Skill.count") do
          post api_v1_console_skills_path, params: {
            data: { name: "release-helper", description: "Help with release readiness.", instructions: "" }
          }, headers: csrf_headers, as: :json
        end

        assert_response :unprocessable_entity
        details = json_body.dig("error", "details")
        assert_equal [ "can't be blank" ], details["instructions"]
        assert_nil details["name"]
        assert_nil details["description"]
      end

      test "rejects an invalid or duplicate name" do
        post api_v1_console_skills_path, params: {
          data: { name: skills(:admin_shared).name, description: "dupe", instructions: "x" }
        }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_includes json_body.dig("error", "details", "name"), "is already used by another skill"

        post api_v1_console_skills_path, params: { data: { name: "Bad Name", description: "d", instructions: "x" } },
                                 headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "name").present?
      end

      test "a stale lock_version is a 409 and saves nothing" do
        skill = skills(:member_private)
        stale = skill.lock_version
        skill.update!(description: "Changed elsewhere.")

        patch api_v1_console_skill_path(skill.oid), params: skill_body(skill, description: "Mine.", lock_version: stale),
                                            headers: csrf_headers, as: :json
        assert_response :conflict
        assert_equal "stale", json_body.dig("error", "details", "reason")
        assert_equal Console::SkillsController::STALE_MESSAGE, json_body.dig("error", "message")
        assert_equal "Changed elsewhere.", skill.reload.description
      end

      test "editor candidates are other active users, filterable by name or email" do
        get editor_candidates_api_v1_console_skills_path
        assert_response :ok
        emails = json_body["data"].map { |u| u["email"] }
        assert_includes emails, users(:globex_admin).email
        assert_not_includes emails, @user.email
        assert_not_includes emails, users(:disabled_user).email
        assert_not_includes emails, users(:pending_user).email
        assert_equal emails.sort, emails

        get editor_candidates_api_v1_console_skills_path(q: users(:globex_admin).email.upcase)
        assert_equal [ users(:globex_admin).oid ], json_body["data"].map { |u| u["id"] }
      end
    end
  end
end
