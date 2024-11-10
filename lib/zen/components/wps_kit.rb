# typed: true
# frozen_string_literal: true

module Zen
  module Components
    require "fileutils"
    require "active_support/all"
    require "digest"
    require "rake/file_list"

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
        md5_digest = sha2_hash.hexdigest

        OpenStruct.new(file_hash: md5_digest, block_count:, file_stat: "")
      end

      ##
      # dotfiles , backup and restore USER_HOME dot config file
      def dotfiles
        # os home directory
        home = Dir.home
        archive_root = File.join(home, "Documents/Archive")

        dotpath = "dotfiles"

        file_list = [
          ".zshrc",
          ".zshenv",
          ".zprofile",
          ".profile",
          ".gitconfig",
          ".ssh",
          ".m2/setting.xml",
          ".rbenv/version",
          ".pyenv/version",
          ".vimrc",
          ".vim",
          ".config/gem",
          ".config/gh",
          ".config/pip"
        ]

        file_list.each do |file|
          src_file = File.join(Dir.home, file)
          to_file = File.join(archive_root, dotpath, file)

          to_dir = File.dirname(to_file)
          puts "dotfiles:#{src_file}, #{to_file}"

          FileUtils.makedirs(to_dir) unless File.exist?(to_dir)

          next unless File.exist?(src_file)

          # system("cp -fr #{src_file}  #{to_dir}")
          FileUtils.cp_r(src_file, to_dir, remove_destination: true)
        end
        puts "dotfiles done."
      end

      #
      # check command exists
      # @return [boolean]
      def command_exists?(command)
        system("which #{command} > /dev/null 2>&1")
        $?.success?
      end

      ##
      # Use tar  and zstd compression and decompression directory
      def zstds(from_dir, output_filename)
        zstd_available = command_exists?("zstd")

        if zstd_available
          # puts "zstd command is available."
        else
          puts "zstd command is not available. use `brew install zstd` to install zstd."
        end

        from_path = File.dirname(from_dir)

        from_name = File.basename(from_dir)

        # Use zstd if desired, otherwise comment out the line

        puts("tar -cf - -C #{from_path} #{from_name} | zstd -f > #{output_filename}")

        system("tar -cf - -C #{from_path} #{from_name} | zstd -f > #{output_filename}")

        # Alternative for gzip compression (replace zstd with gzip)
        # system("tar -cf - #{_from} | gzip > #{output_filename}")
      end

      ##
      # Use tar and zstd compression and archive worksapce directory
      def archive(from_dir, _output_filename)
        require "date"

        # os home directory
        home = Dir.home

        file_list = [from_dir]

        if from_dir.nil?
          file_list = [
            # File.join(home, "CodeRepo/ownspace"),
            # File.join(home, "CodeRepo/funspace"),
            # File.join(home, "CodeRepo/acespace"),
            # File.join(home, "CodeRepo/workspace"),
            File.join(home, "Documents/Work"),
            File.join(home, "Documents/Other"),
            File.join(home, "Documents/Personal"),
            File.join(home, "Documents/Book")
          ]
        end

        archive_path = File.join(home, "Documents/Archive")

        file_list.each do |file|
          file_name = File.basename(file)

          # puts File.path(file)

          next unless File.exist?(file)

          now = Date.today

          now_date = now.strftime("%Y%m%d")

          archive_name = "#{file_name}-#{now_date}.tar.zst"

          # archive_name
          archive_file = File.join(archive_path, archive_name)

          puts "archive #{file}  to #{archive_file}"
          # zstd file
          zstds(file, archive_file)
        end
      end

      def unixtime(timestamp, _timeunit)
        now = if timestamp.nil?
                # 获取当前时间的长时间戳
                DateTime.now
              else
                Time.at timestamp.to_d
              end

        seconds = now.to_i

        puts "Current Time: #{now}"

        puts "Seconds since Epoch: #{seconds}"

        microseconds = now.usec

        # 计算总的微秒值
        microseconds_epoch = (seconds * 1_000_000) + microseconds

        # 将秒数转换为毫秒，并加上微秒数的千分之一
        milliseconds = (seconds * 1_000) + (microseconds / 1_000)

        nanoseconds = now.nsec

        nanoseconds_epoch = (seconds * 1_000_000_000) + nanoseconds

        puts "Milliseconds since Epoch: #{milliseconds}"

        puts "Microseconds since Epoch: #{microseconds_epoch}"

        puts "Nanoseconds since Epoch: #{nanoseconds_epoch}"

        puts "Current Time UTC: #{now.utc}"

        # 格式化时间戳
        formatted_time = now.localtime.strftime("%Y-%m-%d %H:%M:%S.%6N %z")
        puts "Local Date Time: #{formatted_time}"
      end

      def giturl(from_url)
        # 使用URI.parse来解析URL
        uri = URI.parse(from_url)

        # 检查是否是合法的HTTP或HTTPS URL
        unless uri.scheme == "http" || uri.scheme == "https"
          raise ArgumentError, "Invalid URL: URL must be HTTP or HTTPS"
        end

        # 获取主机名和路径
        host = uri.host
        path = uri.path

        # 移除路径的前导斜杠
        path.slice!(0) if path.start_with?("/")

        # 组装SSH URL
        ssh_url = "git@#{host}:#{path}"
        ssh_url.sub!(".git", "") unless path.end_with?(".git") # 确保路径以.git结尾
        ssh_url << ".git" unless ssh_url.end_with?(".git") # 确保路径以.git结尾

        ssh_url
        puts "\tssh  : #{ssh_url}"
      end
    end
  end
end
