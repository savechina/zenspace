# typed: true
# frozen_string_literal: true

module Zen
  module Components
    # Member Kit
    class MemberKit
      require "zen/system/import"

      # include Import["logger"]
      require "sequel"

      Application.logger.info("memberkit init")

      Configuration.configurate

      df_file = "test.db" if Settings.zen.data.db.nil?

      # puts "#{Settings.zen.data.db}"

      DB = Sequel.sqlite(df_file)

      # require_relative "member/member"

      def init; end
    end
  end
end
