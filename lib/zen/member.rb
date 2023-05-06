module Zen
  # Member Kit
  module MemberKit
    require "sequel"

    Configuration.configurate

    df_file = "test.db" if Settings.zen.data.db.nil?

    puts "#{Settings.zen.data.db}"

    DB = Sequel.sqlite(df_file)

    require_relative "member/member"

    def init; end
  end
end
