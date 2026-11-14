namespace A { class Enclosed {}; }
namespace C { class Enclosed {}; }

// C++ looks for `Enclosed` in `A::B`, then in `A`, so it is `A::Enclosed`.
namespace A::B { class Child : public Enclosed {}; }
