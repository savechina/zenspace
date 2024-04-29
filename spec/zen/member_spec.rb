# frozen_string_literal: true

require "sequel"

RSpec.describe Zen::MemberKit do
  it "does something useful" do
    member = Zen::MemberKit::Member.new
    member.name = "wan"
    member.age = 37
    member.id = 0
    member.rank = Zen::MemberKit::Member::RANKS[0]

    puts "member name #{member.name},age #{member.age}"

    expect(member.name).to eq("wan")
  end

  context "member db" do
    f = File.absolute_path("../config/settings.yml", __FILE__)
    puts "absolute_path #{f}"

    f = File.expand_path("config/settings.yml", __dir__)
    puts "expand_path #{f}"

    puts "local: #{__dir__}"

    # connect to an in-memory database
    DB = Sequel.sqlite("test.db")

    is = DB.supports_create_table_if_not_exists?

    it "does members list" do
      puts "supports_create_table_if_not_exists #{is}"

      # create an items table
      DB.create_table! :items do
        primary_key :id
        String :name, unique: true, null: false
        Float :price, null: false
      end

      # create a dataset from the items table
      items = DB[:items]

      # populate the table
      items.insert(name: "abc", price: rand * 100)
      items.insert(name: "def", price: rand * 100)
      items.insert(name: "ghi", price: rand * 100)
      items.insert(name: "jkl", price: rand * 100)

      # print out the number of records
      puts "Item count: #{items.count}"

      # print out the average price
      puts "The average price is: #{items.avg(:price)}"

      class Item < Sequel::Model(items.where(name: "abc")); end
      ii = Item.first

      puts "Item have abc is price #{ii.price}"
    end

    it "does model sample" do
      # init db create person table  if exists and drop table
      DB.create_table! :zen_persons do
        primary_key :id
        String :name, unique: true, null: false
        Float :price, null: false
        Integer :sex, null: false
      end

      class Person < Sequel::Model(DB[:zen_persons])
      end

      # Person.dataset.destroy
      # person = Person.find_or_create(name: "wan") do |p|
      #   p.price = rand * 10_000
      #   p.sex = 1
      # end

      # puts person.values
    end
  end
end
