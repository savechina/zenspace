# typed: false

require "zen/user"
require "faker"
FactoryBot.define do
  factory :user do
    name { Faker::Name.name }
    email { Faker::Internet.email }
    skip_create
  end
end
