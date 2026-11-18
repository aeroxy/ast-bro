defmodule Shop.Pricing do
  alias Shop.Rules

  # `Rules.Tax` expands through the alias. `Shop.Rules` itself is defined
  # nowhere, so it stays external rather than matching a prefix.
  def tax(x), do: Rules.Tax.rate(x)

  # A module nested in another file's module resolves to that file.
  def sum(lines), do: Enum.sum(Enum.map(lines, &Shop.Cart.Line.price/1))
end
