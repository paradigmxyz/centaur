module Api
  module V1
    module Sandbox
      module Admin
        class GranteeGrantsController < ::Api::V1::GranteeGrantsController
          include SandboxAdminAuthentication
        end
      end
    end
  end
end
