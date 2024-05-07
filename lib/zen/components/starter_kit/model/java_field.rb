# frozen_string_literal: true
# typed:true

module Zen
  module Components
    require "dry-struct"

    class StarterKit
      ##
      # Java Object
      class JavaField < Dry::Struct
        attribute :field_name, Types::String

        attribute :field_type, Types::String

        attribute :comment, Types::String

        attribute :column_name, Types::String

        attribute :column_type, Types::String
      end
    end
  end
end
