# typed:true
# frozen_string_literal: true

module Zen
  module Components
    class StarterKit
      module Model
        ##
        # Java Project
        class JavaProject
          # Project Name
          # @return [String]
          attr_accessor :project_name

          # @return [String]
          attr_accessor :group_name

          # @return [String]
          attr_accessor :package_name

          # Project Comment
          # @return [String]
          attr_accessor :project_comment

          # arch type
          # @return [String]
          attr_accessor :arch_type
        end
      end
    end
  end
end
