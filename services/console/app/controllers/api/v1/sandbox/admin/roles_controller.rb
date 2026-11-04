module Api
  module V1
    module Sandbox
      module Admin
        class RolesController < ::Api::V1::RolesController
          include SandboxAdminAuthentication
        end
      end
    end
  end
end
