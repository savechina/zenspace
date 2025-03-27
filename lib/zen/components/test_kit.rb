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
    # require_relative "test_kit/model/user"

    ##
    # TestKit
    #
    class TestKit
      def init
        puts "test init"

        puts File.expand_path("test_kit/factories", __dir__)
        # 指定 factories 目录
        FactoryBot.definition_file_paths = [File.expand_path("test_kit/factories", __dir__)]

        FactoryBot.find_definitions

        # 检查 Factory 是否注册成功
        puts FactoryBot.factories.map(&:name) # 应包含 :product 和 :warehouse_item

        # This will guess the User class
        user = FactoryBot.build(:user)

        puts user.first_name
        puts user.email

        # This will guess the Sku class
        sku = FactoryBot.build(:sku)
        pp sku
      end
    end
  end
end
