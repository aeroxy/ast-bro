#!/usr/bin/env elixir
# A standalone script: top-level code plus a helper module.
Mix.install([{:jason, "~> 1.4"}])

alias Billing.Account

defmodule Script.Helper do
  def greet(name), do: "hello #{name}"
end

IO.puts(Script.Helper.greet("world"))
