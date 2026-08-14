class Root:
    pass


class Other:
    pass


class Leaf(Root):
    pass


class Hard(Root, Other, metaclass=type):
    """Several bases and a keyword argument that is not one."""
