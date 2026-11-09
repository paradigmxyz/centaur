# A principal an API client may run sessions as. At most one per client is the
# default, used when a session does not name a principal.
class ApiClientPrincipal < ApplicationRecord
  oid_prefix "aclp"

  belongs_to :api_client
  belongs_to :principal

  validates :principal_id, uniqueness: { scope: :api_client_id, message: "is already attached to this API client" }
end
