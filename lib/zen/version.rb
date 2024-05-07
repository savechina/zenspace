# typed: true
# frozen_string_literal: true

##
#  Semantic Versioning
# @see (https://semver.org/)
#
module Zen
  ##
  # The major version of Zen.  Only bumped for major changes.
  #
  MAJOR = 0

  ##
  # The minor version of Zen.  Bumped for every non-patch level
  # release, generally around once a month.
  #
  MINOR = 1

  ##
  # The tiny version of Zen.  Usually 0, only bumped for bugfix
  # releases that fix regressions from previous versions.
  #
  TINY  = 0

  ##
  # The version of Zen you are using, as a string (e.g. "0.1.0")
  #
  VERSION = [MAJOR, MINOR, TINY].join(".").freeze

  ##
  # The version of Zen you are using, as a number (1.1.0 -> 10010)
  #
  VERSION_NUMBER = (MAJOR * 10_000) + (MINOR * 10) + TINY

  ##
  # print version
  # @return [String] Version
  #
  def self.version
    VERSION
  end
end
