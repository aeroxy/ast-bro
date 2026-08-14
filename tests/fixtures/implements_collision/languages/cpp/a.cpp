struct Root {};

struct Other {};

// `public` is an access_specifier inside the clause, not a base.
class Leaf : public Root {};

template <class T>
struct Wrapper {};

/// Several bases: qualified, templated, and virtually inherited.
class Hard : public Root, virtual protected Other, public Wrapper<int> {};
