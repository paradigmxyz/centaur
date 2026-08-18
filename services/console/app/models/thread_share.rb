class ThreadShare < ApplicationRecord
  belongs_to :created_by, class_name: "User"

  validates :thread_key, presence: true, length: { maximum: 512 }
end
