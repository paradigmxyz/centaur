require "test_helper"

class ThreadShareTest < ActiveSupport::TestCase
  test "the database enforces globally unique thread keys" do
    ThreadShare.create!(thread_key: "console:shared", created_by: users(:acme_admin))
    duplicate = ThreadShare.new(thread_key: "console:shared", created_by: users(:member_user))

    assert_raises(ActiveRecord::RecordNotUnique) { duplicate.save! }
  end

  test "thread keys use the session API maximum length" do
    share = ThreadShare.new(thread_key: "x" * 513, created_by: users(:acme_admin))

    assert_not share.valid?
    assert_includes share.errors[:thread_key], "is too long (maximum is 512 characters)"
  end
end
