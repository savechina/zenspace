# typed: false

require "zen/user"
require "faker"
FactoryBot.define do
  factory :user do
    name { Faker::Name.name }
    # email { Faker::Internet.email }

    sequence(:email) { |n| "person#{n}@example.com" }
    skip_create
  end
end
