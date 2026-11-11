namespace A { public class Enclosed { } }
namespace C { public class Enclosed { } }

// C# looks for `Enclosed` in `A.B`, then in `A`, so it is `A.Enclosed`.
namespace A.B { public class Child : Enclosed { } }
