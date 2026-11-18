defmodule Shop do
  alias Shop.{Cart, Pricing}
  alias Shop.Inventory, as: Stock
  import Shop.Format
  require Logger
  use Shop.Macros

  def total(cart), do: Pricing.sum(Cart.items(cart))
  def stock(item), do: Stock.count(item)
  def checkout(x), do: Shop.Payment.charge(x)
  def receipt(r), do: %Shop.Receipt{lines: r}

  # A typespec and quoted code name modules this file does not depend on.
  @spec ok(Shop.Typed.t()) :: :ok
  def ok(_), do: :ok

  defmacro run(x) do
    quote do
      Shop.Quoted.run(unquote(Shop.Unquoted.arg(x)))
    end
  end
end
