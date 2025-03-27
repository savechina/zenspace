# typed: true
# frozen_string_literal: true

module Zen
  module Components
    class TestKit
      module Model
        ##
        # User Model
        class User
          # name
          attr_accessor :first_name

          attr_accessor :last_name
          # email
          attr_accessor :email

          # def save!; end
        end
      end
    end
  end
end
