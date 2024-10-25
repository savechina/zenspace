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

    ##
    # WpsKit Work Process Suite
    #
    #
    class TestKit
      def init
        puts "test init"
        # FactoryBot.define do
        #   factory :user do
        #     first_name { "Joe" }
        #     last_name  { "Blow" }
        #     email { "#{first_name}.#{last_name}@example.com".downcase }
        #   end
        #   # => "joe.doe@example.com"
        # end

        # This will guess the User class
        # user = FactoryBot.create(:User)

        # puts user.name
        # puts user.email
      end
    end
  end
end
