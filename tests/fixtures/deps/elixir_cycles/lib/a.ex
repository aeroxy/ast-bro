# A runtime cycle: B calls back into A.
defmodule A do
  def f, do: B.g()
end
