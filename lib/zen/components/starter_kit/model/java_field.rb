# typed:true
# frozen_string_literal: true

module Zen
  module Components
    class StarterKit
      module Model
        ##
        # Java Object Field
        class JavaField
          ##
          # field name
          # @return [String]
          attr_accessor :field_name

          # field comment
          # @return [String]
          attr_accessor :field_comment

          # field type: Java Type
          # @return [String]
          # @see [JavaTypes]
          attr_accessor :java_type

          # @return [String]
          attr_accessor :column_name

          ##
          # database column  type
          attr_accessor :table_name

          # @return [String]
          # @see [JavaTypes]
          attr_accessor :column_type

          # type: JDBC Type
          # @return [String]
          # @see [JavaTypes]
          attr_accessor :jdbc_type
        end
      end
    end
  end
end
