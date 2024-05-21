# typed: true
# frozen_string_literal: true

module Zen
  module Components
    class StarterKit
      module Model
        ##
        # Java Class Field
        class JavaContext
          # Project
          # @return [JavaProject]
          attr_accessor :project
          # Model clazz
          # @return [JavaClass]
          attr_accessor :model
          # Modules
          # @return [Array(JavaModule)]
          attr_accessor :modules

          def get_binding
            binding
          end
        end
      end
    end
  end
end
