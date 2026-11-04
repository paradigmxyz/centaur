module Api
  module V1
    module Sandbox
      module Admin
        class PrincipalRolesController < ::Api::V1::PrincipalRolesController
          include SandboxAdminAuthentication
        end
      end
    end
  end
end
