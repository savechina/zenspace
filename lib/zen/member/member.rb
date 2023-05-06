require "sequel"

module Zen
  # Member Kit
  module MemberKit
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
        Integer :state,               null: false;
      end
    end

    # Member Rank Level
    class RankEnum
      T01 = 1
      T02 = 2
      T03 = 3
      T04 = 4
      T05 = 5
      T06 = 6
      T07 = 7
      T08 = 8
      T09 = 9
      T10 = 10
    end

    # 人员性别枚举
    class SexEnum
      MAN = 1
      WOMEN = 2
    end

    class MemberRepo < Sequel::Model(DB[:members])
    end

    # 成员信息
    class Member
      RANKS = %i[RANK_T1 RANK_T2 RANK_T3].freeze

      # member's name
      attr_accessor :name
      # member's id
      attr_accessor :id
      attr_accessor :rank
      attr_accessor :hiredate
      attr_accessor :sex
      attr_accessor :age
      attr_accessor :birthdate

      def initialize(name = [nil])
        @name = name
      end
    end
  end
end
