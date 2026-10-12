defmodule Kernels do
  # `deftyped` stands for a typed DSL's def-like macro; `defn` is Nx's.
  import Nx.Defn

  @doc "Adds two integers."
  deftyped add(a :: integer, b :: integer) :: integer do
    a + b
  end

  deftyped count(n :: integer) :: integer, inline: false do
    n
  end

  deftyped double(x :: float) :: float, do: x * 2.0

  deftyped sign(x :: integer) :: integer when x < 0, do: -1
  deftyped sign(x :: integer) :: integer, do: 1

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
