module Api
  module V1
    module Sandbox
      class SkillsController < Api::SandboxBaseController
        MAX_LIMIT = 20
        before_action :require_authoring_user!, only: %i[create update destroy share unshare add_editor remove_editor]

        def index
          scope = visible_skills.order(updated_at: :desc, id: :asc).limit(limit)
          scope = scope.where(visibility: params[:scope]) if %w[private shared].include?(params[:scope])
          render json: { data: scope.map(&:catalog_payload) }
        end

        def search
          query = params.require(:q).to_s.strip
          raise ActionController::BadRequest, "q must not be empty" if query.blank?

          skills = visible_skills.search(query).limit(limit)
          render json: { data: skills.map(&:catalog_payload) }
        end

        def show
          skill = visible_skill
          response.headers["Cache-Control"] = "no-store"
          render json: { data: skill.catalog_payload(include_document: true) }
        end

        def create
          skill = authoring_user.skills.create!(skill_params.except(:lock_version))
          render json: { data: author_payload(skill) }, status: :created
        end

        def update
          skill = editable_skill
          skill.update!(skill_params)
          render json: { data: author_payload(skill) }
        end

        def destroy
          owned_skill.archive!
          head :no_content
        end

        def share
          skill = owned_skill
          skill.share!
          render json: { data: author_payload(skill) }
        end

        def unshare
          skill = owned_skill
          skill.unshare!
          render json: { data: author_payload(skill) }
        end

        def editors
          render json: { data: editor_management_payload(visible_skill) }
        end

        def add_editor
          skill = owned_skill
          editor = resolve_editor!(User.active)

          Skill.transaction do
            assignment = skill.skill_editors.find_or_initialize_by(user: editor)
            if assignment.new_record?
              assignment.save!
              skill.touch
            end
          end

          render json: { data: editor_management_payload(skill) }
        end

        def remove_editor
          skill = owned_skill
          editor = resolve_editor!(skill.editors)

          Skill.transaction do
            skill.skill_editors.find_by!(user: editor).destroy!
            skill.touch
          end

          render json: { data: editor_management_payload(skill) }
        end

        private

        def require_authoring_user!
          return if authoring_user

          render_error(status: :forbidden, message: "sandbox principal is not linked to an active Console user")
        end

        def visible_skills
          Skill.catalog_visible_to(linked_console_user).includes(:user)
        end

        def visible_skill
          return visible_skills.find_by_oid!(params[:id]) if params[:id].start_with?("skl_")

          visible_skills.find_by!(name: params[:id])
        end

        def linked_console_user
          user = current_proxy.principal.console_user
          user if user&.active?
        end

        def authoring_user
          @authoring_user ||= linked_console_user
        end

        def owned_skill
          authoring_user.skills.active.find_by_oid!(params[:id])
        end

        def editable_skill
          Skill.editable_by(authoring_user).find_by_oid!(params[:id])
        end

        def skill_params
          submitted = params.require(:data).permit(:name, :description, :instructions, :lock_version)
          submitted[:content] = submitted.delete(:instructions) if submitted.key?(:instructions)
          submitted
        end

        def author_payload(skill)
          skill.catalog_payload(include_document: true).merge(lock_version: skill.lock_version)
        end

        def editor_management_payload(skill)
          {
            id: skill.oid,
            editors: skill.editors.order(:email).map { |editor| editor_payload(editor) },
            lock_version: skill.lock_version
          }
        end

        def editor_payload(editor)
          {
            id: editor.oid,
            email: editor.email,
            name: editor.name,
            status: editor.status
          }
        end

        def resolve_editor!(scope)
          reference = params.require(:data).require(:user).to_s.strip
          return scope.find_by_oid!(reference) if reference.start_with?("usr_")

          scope.find_by!(email: reference.downcase)
        end

        def limit
          value = Integer(params.fetch(:limit, 10).to_s, 10)
          value.clamp(1, MAX_LIMIT)
        rescue ArgumentError, TypeError
          raise ActionController::BadRequest, "limit must be an integer"
        end
      end
    end
  end
end
