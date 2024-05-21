# typed: true
# frozen_string_literal: true

require_relative "java_field"
module Zen
  module Components
    class StarterKit
      module Model
        ##
        # Java Object
        class JavaClass
          # Package Name
          # @return [String]
          attr_accessor :package_name

          # Class Name
          # @return [String]
          attr_accessor :class_name

          # Class Comment
          # @return [String]
          attr_accessor :class_comment

          # Table Name
          # @return [String]
          attr_accessor :table_name

          ##
          # Fields
          # @return [Array<JavaField>]
          attr_accessor :fields

          # Feature Name
          # @return [String]
          attr_accessor :feature_name
        end
      end
    end
  end
end
