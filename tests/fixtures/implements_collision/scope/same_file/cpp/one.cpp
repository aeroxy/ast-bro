namespace A { class Shared {}; }

// This `Shared` is `B::Shared` from two.cpp. `A::Shared` is in this
// file but not in scope.
namespace B { class Child : public Shared {}; }
