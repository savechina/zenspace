require_relative "java_field"
module Zen
  module Components
    require "dry-struct"

    class StarterKit
      ##
      # Java Object
      class JavaClass < Dry::Struct
        attribute :package_name, Types::String.optional

        attribute :class_name, Types::String

        attribute :table_name, Types::String

        attribute :fields, Types::Array.of(StarterKit::JavaField)
      end
    end
  end
end
