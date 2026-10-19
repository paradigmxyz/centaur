module Api
  module V1
    class SystemSettingsController < Api::BaseController
      def show
        response.headers["Cache-Control"] = "no-store"
        render json: { data: { slack_external_team_ids: SystemSetting.current.slack_external_team_ids } }
      end
    end
  end
end
