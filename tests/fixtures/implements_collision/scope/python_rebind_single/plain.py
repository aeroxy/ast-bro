# `ext` is not in the walk, and `a.mod` declares the only `Rebound` in it.
from ext.mod import Rebound
from a.mod import Rebound


class Plain(Rebound):
    pass
