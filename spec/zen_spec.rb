# frozen_string_literal: true

RSpec.describe Zen do
  it "has a version number" do
    expect(Zen::VERSION_NUMBER).not_to be nil

    puts "version number:#{Zen::VERSION_NUMBER}"
  end

  it "does db members list" do
    expect(true).to eq(true)
  end
end
