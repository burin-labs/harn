module Fixture
  class Greeter
    def initialize(name)
      @name = name
    end

    def greet
      "hello #{@name}"
    end

    def self.default
      new("world")
    end
  end

  def self.shout(s)
    s.upcase
  end
end
