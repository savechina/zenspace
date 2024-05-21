# typed: true
# frozen_string_literal: true

module Zen
  module Components
    require "fileutils"
    require "active_support/all"
    require "digest"
    ##
    # WpsKit Work Process Suite
    #
    #
    class WpsKit
      ##
      # find file duplicates
      # @reeturn [Hash]
      def find_duplicates(file_regex)
        # file list
        file_unique = {}
        # duplicates file list
        file_dupes = {}

        # foreach file and compare file checksum ,find deplicates file
        Dir.glob("#{file_regex}").each do |file|
          next unless File.file?(file)

          # calculate file checksum
          file_hash, block_count = file_checksum(file)

          if file_unique.has_key? file_hash
            file_dupes[file_hash] = [file] << file_unique[file_hash]
          else
            file_unique[file_hash] = file
          end
        end
        # return
        file_dupes
      end

      ##
      # calculate file checksum
      # @return OpenStruct()
      def file_checksum(file_path)
        # Open the file in binary read mode
        file = File.open(file_path, "rb")

        # Initialize variables for SHA256 hash and block count
        sha2_hash = Digest::SHA2.new(256)
        # file block count
        block_count = 0

        # Read the file in 4096-byte blocks
        block_size = 4096
        while block = file.read(block_size)
          # Update the MD5 hash with each block
          sha2_hash.update(block)

          # Increment the block count
          block_count += 1
        end

        # Close the file
        file.close

        # Get the final MD5 hash digest
        md5_digest = md5_hash.hexdigest

        OpenStruct.new(file_hash: md5_digest, block_count:, file_stat: "")
      end
    end
  end
end
