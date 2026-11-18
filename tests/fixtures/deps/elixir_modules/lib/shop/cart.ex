defmodule Shop.Cart do
  alias __MODULE__.Line

  def items(cart), do: Enum.map(cart, &Line.new/1)

  defmodule Line do
    defstruct [:sku, :price]
    def new(sku), do: %__MODULE__{sku: sku}
  end
end
