class DummyNonMonotonicCiFailure < ActiveRecord::Migration[8.0]
  def change
    # Dummy migration for validating CI migration-order failure.
  end
end
