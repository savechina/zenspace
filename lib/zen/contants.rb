# typed: true
# frozen_string_literal: true

##
# Zen Contants
module Zen
  # The template root path for Zen templates
  TEMPLATE_ROOT = File.join(ROOT, "templates")

  # user root data dir
  USER_ROOT_DATA = File.join(USER_ROOT, "data")

  # templates root workspace
  USER_TEMPLATE_ROOT = File.join(USER_ROOT, "templates")
end
