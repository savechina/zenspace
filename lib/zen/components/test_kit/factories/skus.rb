# typed: true
# frozen_string_literal: true

module Zen
  module Components
    class TestKit
      ##
      # Test Kit Factories
      module Factories
        ##
        # Sku Facotry
        FactoryBot.define do
          factory :sku, class: Model::Sku do
            name { Faker::Commerce.product_name }
            # email { "#{first_name}.#{last_name}@example.com".downcase }
            skip_create
          end
        end
      end
    end
  end
end
