# `ext` is not in the walk.
from a.mod import Rebound
from ext.mod import Rebound


class Plain(Rebound):
    pass
