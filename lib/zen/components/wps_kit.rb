# typed: true
# frozen_string_literal: true

module Zen
  module Components
    require "fileutils"
    require "active_support/all"
    require "digest"
    ##
    # StarterKit
    #
    class WpsKit
      ##
      # find file duplicates
      def find_duplicates(file_regex)
        file_unique = {}
        file_dupes = {}
        Dir.glob("#{file_regex}").each do |file|
          next unless File.file?(file)

          file_hash, block_count = file_checksum(file)

          if file_unique.has_key? file_hash
            file_dupes[file_hash] = [file] << file_unique[file_hash]
          else
            file_unique[file_hash] = file
          end
        end

        # "file : #{pp file_unique}"
        file_dupes
      end

      def file_checksum(file_path)
        # Open the file in binary read mode
        file = File.open(file_path, "rb")

        file_stat = file.stat

        # Initialize variables for SHA256 hash and block count
        md5_hash = Digest::MD5.new
        md5_hash = Digest::SHA2.new(256)
        block_count = 0

        # Read the file in 4096-byte blocks
        block_size = 4096
        while block = file.read(block_size)
          # Update the MD5 hash with each block
          md5_hash.update(block)

          # Increment the block count
          block_count += 1
        end

        # Close the file
        file.close

        # Get the final MD5 hash digest
        md5_digest = md5_hash.hexdigest

        OpenStruct.new(file_hash: md5_digest, block_count:, file_stat: "")
      end

      def file_checksum2(file)
        cmd = "cksum #{file}"

        require "open3"

        stdout, stderr, status = Open3.capture3(cmd)

        puts "Standard Output:\n#{stdout}"

        regex = /^(\d+) (\d+) (.*)$/

        match = regex.match(stdout)

        if match
          userID = match[1]
          projectID = match[2]
          filePath = match[3]

          puts "User ID: #{userID}"
          puts "Project ID: #{projectID}"
          puts "File Path: #{filePath}"
        else
          puts "No match found"
        end
      end
    end
  end
end
