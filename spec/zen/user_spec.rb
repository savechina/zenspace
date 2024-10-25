# typed: false

require "zen"
require "zen/user"
# require "rails_helper"
RSpec.describe User do
  it "is valid with valid attributes" do
    user = FactoryBot.create(:user)
    expect(user)
  end

  it "is invalid without a name" do
    user = FactoryBot.build(:user, name: nil)
    expect(user)
  end
end
