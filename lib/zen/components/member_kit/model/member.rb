# typed:true
# frozen_string_literal: true

require "sequel"

module Zen
  module Components
    # Member Kit
    class MemberKit
      module Model
        # 初始化数据表
        unless DB.table_exists?(:members)
          DB.create_table?(:members) do
            # 编号
            primary_key :id
            # ERP
            String  :nick, unique: true,  null: false
            # 姓名
            String  :name,                null: false
            # 性别
            Integer :sex,                 null: false
            # 年龄
            Integer :age,                 null: false
            # 雇佣日期
            Date    :hiredate,            null: true
            # 出生日期
            Date    :birthdate,           null: true
            # 状态
            Integer :state,               null: false
          end
        end

        class MemberRepo < Sequel::Model(DB[:members])
        end

        # 成员信息
        class Member
          # member's name
          attr_accessor :name
          # member's id
          attr_accessor :id
          attr_accessor :rank, :hiredate, :sex, :age, :birthdate

          def initialize(name = [nil])
            @name = name
          end
        end
      end
    end
  end
end
