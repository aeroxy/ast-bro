# The protocol and every `for:` type are dependencies.
defimpl Shop.Proto, for: [Shop.Payment, Shop.Receipt] do
  def show(x), do: x
end
