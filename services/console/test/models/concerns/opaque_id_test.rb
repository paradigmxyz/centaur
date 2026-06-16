require "test_helper"

class OpaqueIdTest < ActiveSupport::TestCase
  # Backing classes for the concern's behavior. They share the `principals`
  # table but declare distinct prefixes to exercise per-prefix encoding.
  class PrnModel < ApplicationRecord
    self.table_name = "principals"
    oid_prefix "prn"
  end

  class GrantModel < ApplicationRecord
    self.table_name = "principals"
    oid_prefix "grant"
  end

  class UndeclaredModel < ApplicationRecord
    self.table_name = "principals"
  end

  def create_record(klass = PrnModel)
    klass.create!(
      namespace: "test",
      foreign_id: "f-#{SecureRandom.hex(6)}",
      created_by_id: users(:acme_admin).id
    )
  end

  test "oid_prefix returns the declared prefix" do
    assert_equal "prn", PrnModel.oid_prefix
    assert_equal "grant", GrantModel.oid_prefix
  end

  test "oid_prefix raises NotImplementedError when not declared" do
    assert_raises(NotImplementedError) { UndeclaredModel.oid_prefix }
  end

  test "oid_prefix rejects blank prefixes" do
    assert_raises(ArgumentError) do
      Class.new(ApplicationRecord) { self.table_name = "principals"; oid_prefix "" }
    end
  end

  test "oid_prefix rejects prefixes containing underscores" do
    assert_raises(ArgumentError) do
      Class.new(ApplicationRecord) { self.table_name = "principals"; oid_prefix "foo_bar" }
    end
  end

  test "oid is nil for unpersisted records" do
    assert_nil PrnModel.new.oid
  end

  test "oid is prefix-underscore-encoded for persisted records" do
    record = create_record
    assert_match(/\Aprn_[A-Za-z0-9]+\z/, record.oid)
    assert_operator record.oid.length, :>=, "prn_".length + OpaqueId::MIN_LENGTH
  end

  test "find_by_oid round-trips" do
    record = create_record
    assert_equal record, PrnModel.find_by_oid(record.oid)
  end

  test "decode_oid returns the bigint id for canonical input" do
    record = create_record
    assert_equal record.id, PrnModel.decode_oid(record.oid)
  end

  test "find_by_oid returns nil for malformed input" do
    assert_nil PrnModel.find_by_oid(nil)
    assert_nil PrnModel.find_by_oid("")
    assert_nil PrnModel.find_by_oid("not-a-prn")
    assert_nil PrnModel.find_by_oid("prn_")
    assert_nil PrnModel.find_by_oid("prn_!!!invalid!!!")
  end

  test "find_by_oid rejects wrong prefix" do
    record = create_record
    encoded = record.oid.delete_prefix("prn_")
    assert_nil PrnModel.find_by_oid("grant_#{encoded}")
  end

  test "find_by_oid returns nil when decoded value is out of Sqids range" do
    # "doesnotexist" decodes to a number larger than Sqids' max (2**62 - 1),
    # which makes encode raise ArgumentError. decode_oid must rescue that.
    assert_nil PrnModel.find_by_oid("prn_doesnotexist")
  end

  test "find_by_oid! raises ActiveRecord::RecordNotFound on miss" do
    assert_raises(ActiveRecord::RecordNotFound) do
      PrnModel.find_by_oid!("prn_doesnotexist")
    end
  end

  test "different prefixes produce different encodings for the same id" do
    prn = create_record(PrnModel)
    grant = GrantModel.find(prn.id)
    assert_not_equal prn.oid.delete_prefix("prn_"), grant.oid.delete_prefix("grant_")
  end

  test "an oid encoded under one prefix does not decode under another" do
    record = create_record(PrnModel)
    encoded = record.oid.delete_prefix("prn_")
    # Even with the right prefix, the encoded body itself won't round-trip
    # under GrantModel's distinct alphabet.
    assert_nil GrantModel.find_by_oid("grant_#{encoded}")
  end
end
