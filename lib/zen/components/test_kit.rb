# typed: true
# frozen_string_literal: true

module Zen
  module Components
    # require "fileutils"
    # require "active_support/all"
    # require "digest"
    # require "rake/file_list"
    require "factory_bot"
    require "faker"
    require_relative "test_kit/model/user"
    ##
    # WpsKit Work Process Suite
    #
    #
    class TestKit
      def init
        puts "test init"

        FactoryBot.define do
          factory :user do
            first_name { Faker::Name.name }
            last_name  { "Blow" }
            email { "#{first_name}.#{last_name}@example.com".downcase }
            skip_create
          end
          # => "joe.doe@example.com"
        end

        # This will guess the User class
        user = FactoryBot.create(:user)

        puts user.first_name
        puts user.email
      end
    end
  end
end
