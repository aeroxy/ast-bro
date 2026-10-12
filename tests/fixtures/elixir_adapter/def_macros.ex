defmodule Kernels do
  use Defk
  import Nx.Defn

  @doc "Adds two integers."
  defk add(a :: i64, b :: i64) :: i64 do
    a + b
  end

  defk count(n :: i64) :: i64, yield: false do
    n
  end

  defk double(x :: f64) :: f64, do: x * 2.0

  defk sign(x :: i64) :: i64 when x < 0, do: -1
  defk sign(x :: i64) :: i64, do: 1

  defn softmax(t) do
    Nx.exp(t) / Nx.sum(Nx.exp(t))
  end

  defnp helper(t), do: t

  defn zero do
    0
  end

  # Not definitions: no head, or no body.
  defsearch(:search, [])
  defoverridable add: 2
  define(mod, "+")
  defstep(:a)
end
