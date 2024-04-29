# 引入Thor库，用于创建命令行接口
require "thor"

# 定义一个插件模块，用于存放各种插件类
module Plugins
  # 定义一个插件基类，用于提供插件的公共方法和属性
  class Plugin
    # 定义一个类方法，用于注册插件到命令行工具中
    def self.register(tool)
      # 获取插件的名称和描述
      name = const_get(:NAME)
      desc = const_get(:DESC)
      # 调用命令行工具的desc方法，设置子命令的名称和描述
      tool.desc(name, desc)
      # 调用命令行工具的method_option方法，设置子命令的选项
      options.each do |key, value|
        tool.method_option(key, value)
      end
      # 调用命令行工具的define_method方法，定义子命令的执行逻辑
      tool.define_method(name) do
        # 创建插件对象，传入选项参数
        plugin = initialize(options)
        # 调用插件对象的run方法，执行插件逻辑
        plugin.run
      end
    end

    # 定义一个类方法，用于获取插件的选项，返回一个哈希表
    def self.options
      # 如果插件类定义了OPTIONS常量，就返回它，否则返回一个空哈希表
      const_defined?(:OPTIONS) ? const_get(:OPTIONS) : {}
    end

    # 定义一个实例方法，用于初始化插件对象，接受一个选项参数
    def initialize(options)
      # 将选项参数赋值给实例变量
      @options = options
      self
    end

    # 定义一个抽象的实例方法，用于执行插件逻辑，由子类实现
    def run
      raise NotImplementedError, "You must implement #{self.class}#run"
    end
  end

  # 定义一个插件子类，用于实现一个问候功能
  class Greet < Plugin
    # 定义插件的名称和描述
    NAME = "greet"
    DESC = "Say hello to someone"

    # 定义插件的选项，包括名称、类型、默认值和描述
    OPTIONS = {
      name: {
        type: :string,
        default: "world",
        desc: "The name of the person to greet"
      }
    }

    # 重写父类的run方法，实现插件逻辑
    def run
      # 获取选项参数中的name值
      name = @options[:name]
      # 输出问候语
      puts "Hello, #{name}!"
    end
  end

  # 定义一个插件子类，用于实现一个计算功能
  class Calc < Plugin
    # 定义插件的名称和描述
    NAME = "calc"
    DESC = "Perform a simple arithmetic operation"

    # 定义插件的选项，包括名称、类型、必需性和描述
    OPTIONS = {
      x: {
        type: :numeric,
        required: true,
        desc: "The first operand"
      },
      y: {
        type: :numeric,
        required: true,
        desc: "The second operand"
      },
      op: {
        type: :string,
        required: true,
        desc: "The operator (+ - * /)"
      }
    }

    # 重写父类的run方法，实现插件逻辑
    def run
      # 获取选项参数中的x, y, op值
      x = @options[:x]
      y = @options[:y]
      op = @options[:op]
      # 根据运算符，执行相应的运算，捕获可能的异常
      begin
        case op
        when "+"
          result = x + y
        when "-"
          result = x - y
        when "*"
          result = x * y
        when "/"
          result = x / y
        else
          raise ArgumentError, "Invalid operator: #{op}"
        end
        # 输出运算结果
        puts "#{x} #{op} #{y} = #{result}"
      rescue StandardError => e
        # 输出异常信息
        puts e.message
      end
    end
  end
end

# 定义一个命令行工具类，继承自Thor
class Tool < Thor
  # 遍历插件模块中的所有插件类，调用它们的register方法，将它们注册到命令行工具中
  Plugins.constants.each do |name|
    plugin = Plugins.const_get(name)
    plugin.register(self) if plugin.is_a?(Class) && plugin < Plugins::Plugin
  end
end

# 调用命令行工具类的start方法，开始执行命令行程序
Tool.start(ARGV)
