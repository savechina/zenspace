# typed:true
# frozen_string_literal: true

require "dry/types"
require "dry-struct"

module Zen
  module Components
    class StarterKit
      module Model
        Types = Dry.Types()
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
end
