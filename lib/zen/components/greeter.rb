# typed: true
# frozen_string_literal: true

module Zen
  module Components
    class Greeter
      def call(name)
        "Greeter:Hello #{name}"
      end
    end
  end
end
