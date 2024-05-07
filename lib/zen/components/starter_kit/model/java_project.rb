# frozen_string_literal: true
# typed:true

require "dry/types"
require "dry-struct"

module Zen
  module Components
    module Types
      include Dry.Types()
    end

    class StarterKit
      ##
      # Java Project
      class JavaProject < Dry::Struct
        attribute :project_name, Types::String
        attribute :group_name, Types::String
        attribute :package_name, Types::String
      end
    end
  end
end
