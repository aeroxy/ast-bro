class Root
end

module Other
end

# The `superclass` node spans the `<` as well as the name.
class Leaf < Root
end

module Ns
  class Nested
  end
end

# A scope-resolved superclass, with a comment between the operator and it.
class Hard < # which one?
    Ns::Nested
end
