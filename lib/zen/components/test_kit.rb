# typed: true
# frozen_string_literal: true

module Zen
  module Components
    # require "fileutils"
    # require "active_support/all"
    # require "digest"
    # require "rake/file_list"
    require "factory_bot"
    # require "faker"

    class User
    end

    ##
    # WpsKit Work Process Suite
    #
    #
    class TestKit
      def init
        puts "test init"

        # This will guess the User class
        # FactoryBot.define do
        #   factory :user do
        #     first_name { "Joe" }
        #     last_name  { "Blow" }
        #     email { "#{first_name}.#{last_name}@example.com".downcase }
        #   end

        #   create(:user, last_name: "Doe").email
        #   # => "joe.doe@example.com"
        # end

        # user = FactoryBot.create(:user)
        # puts user.name
        # puts user.email
      end
    end
  end
end
