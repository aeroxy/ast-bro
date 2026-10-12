defmodule Billing.Account do
  @moduledoc """
  Accounts and their balances.
  """
  @behaviour Billing.Ledger

  alias Billing.{Invoice, Payment}
  alias Billing.Currency, as: Cur
  import Enum, only: [map: 2]
  require Logger
  use GenServer, restart: :temporary

  @default_limit 1_000

  defstruct [:id, :owner, balance: 0]

  @type t :: %__MODULE__{id: integer(), owner: String.t(), balance: integer()}
  @typep cents :: non_neg_integer()

  @doc "Opens an account."
  @spec open(integer(), String.t()) :: t()
  def open(id, owner), do: %__MODULE__{id: id, owner: owner}

  @doc """
  Applies a charge.

  Multi-line docs survive.
  """
  @spec charge(t(), integer()) :: {:ok, t()} | {:error, :limit}
  def charge(%__MODULE__{} = acct, amount) when amount > @default_limit do
    {:error, :limit}
  end

  def charge(acct, amount) do
    {:ok, %{acct | balance: acct.balance + amount}}
  end

  def charge(acct), do: charge(acct, 0)

  @doc false
  def internal_hook(acct), do: acct

  @impl true
  def init(state), do: {:ok, state}

  @impl Billing.Ledger
  def post(entry), do: entry

  @deprecated "Use charge/2 instead"
  def bill(acct, amount), do: charge(acct, amount)

  defp round_cents(x) when is_integer(x), do: x
  defp round_cents(x), do: round(x)

  defmacro audit(expr) do
    quote do: unquote(expr)
  end

  defmacrop trace(expr), do: expr

  defguard is_positive(x) when is_integer(x) and x > 0
  defguardp is_tiny(x) when x < 10

  defdelegate format(amount), to: Billing.Currency

  defmodule Statement do
    @moduledoc false

    def render(acct), do: inspect(acct)
  end
end

defmodule Billing.Ledger do
  @moduledoc "A ledger behaviour."

  @doc "Posts an entry."
  @callback post(entry :: term()) :: term()
  @macrocallback audit(Macro.t()) :: Macro.t()
end

defprotocol Billing.Printable do
  @doc "Renders a value."
  def print(value)
end

defimpl Billing.Printable, for: Billing.Account do
  def print(acct), do: "Account #{acct.id}"
end

defmodule Billing.Error do
  defexception [:message]
end
