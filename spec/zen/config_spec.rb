RSpec.describe Zen::Configuration do
  it "does zen load configuation" do
    config_root = File.absolute_path("config", __FILE__)

    puts config_root.to_s

    config = Zen::Configuration.configurate(config_root, "test")

    # puts "member name #{member.name},age #{member.age}"

    expect(config.has_key?(:zen)).to eq(true)

    puts Settings.has_key?(:zen).to_s

    puts File.expand_path("~/.config/settings.yml").to_s

    config.zen.members = { "db" => "hello.db", "database" => "data" }

    # puts config.to_yaml

    f = File.absolute_path("../config/settings.yml", __FILE__)

    puts File.exist?(f)

    puts config.to_json
    # YAML.add_tag=nil
    fc = config.to_yaml({ indentation: 2, header: true }).gsub("!ruby/object:Config::Options", "")
    puts fc

    # puts YAML.parse(config.to_json).dump
    File.write(f, fc)
  end
end
