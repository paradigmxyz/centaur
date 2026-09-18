class OrganizationInstructionVersion < ApplicationRecord
  oid_prefix "oiv"

  belongs_to :published_by, class_name: "User"

  validates :content, length: { maximum: 32_000 }
end
