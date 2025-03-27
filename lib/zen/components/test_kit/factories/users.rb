# typed: true
# frozen_string_literal: true

module Zen
  module Components
    class TestKit
      ##
      # Test Kit Factories
      module Factories
        ##
        # User Facotry
        FactoryBot.define do
          factory :user, class: Model::User do
            first_name { Faker::Name.first_name }
            last_name  { Faker::Name.last_name }
            email { "#{first_name}.#{last_name}@example.com".downcase }
            skip_create
          end
        end
      end
    end
  end
end
