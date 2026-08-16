# A superclass Ruby computes rather than names. Neither `Struct.new` nor
# `Data.define` is a constant, and both are ordinary in real code.
class Computed < Struct.new(:a, :b)
end

class Shaped < Data.define(:x)
end
