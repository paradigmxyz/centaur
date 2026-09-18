module Api
  module V1
    module Sandbox
      class RuntimeInstructionsController < Api::SandboxBaseController
        def show
          version = SystemSetting.current.published_organization_instruction_version
          content = version&.content.to_s
          body = {
            data: {
              revision: version&.id&.to_s,
              content: content,
              sha256: Digest::SHA256.hexdigest(content),
              published_at: version&.created_at&.iso8601
            }
          }.to_json

          response.headers["ETag"] = %("#{Digest::SHA256.hexdigest(body)}")
          response.headers["Cache-Control"] = "no-store"
          render json: body
        end
      end
    end
  end
end
