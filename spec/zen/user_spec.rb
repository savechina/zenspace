# typed: false

require "zen"
require "zen/user"
# require "rails_helper"
RSpec.describe User do
  it "is valid with valid attributes" do
    user = FactoryBot.create(:user)
    pp user

    created_users = FactoryBot.create_list(:user, 25)

    pp created_users
    expect(user)
  end

  it "is invalid without a name" do
    user = FactoryBot.build(:user, name: nil)

    pp user

    users = build_list(:user, 10)
    pp users
    expect(user)
  end
end
