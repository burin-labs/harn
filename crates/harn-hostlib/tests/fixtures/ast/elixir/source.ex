defmodule Fixture.Greeter do
  def greet(name) do
    "hello #{name}"
  end

  defp internal_helper(s) do
    s
  end
end

defmodule Fixture.Shout do
  def shout(s), do: String.upcase(s)
end
