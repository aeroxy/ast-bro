# A directive cycle: C and D import each other.
defmodule C do
  import D
end
