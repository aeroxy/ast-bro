namespace A { public class Shared { } }

// This `Shared` is `B.Shared` from two.cs. `A.Shared` is in this
// file but not in scope.
namespace B { public class Child : Shared { } }
