using Root = B.Root;

namespace Named
{
    // The alias is named after the type it aliases, which is why C# aliases
    // exist. `real == simple` here, so this is not a rename to the resolver
    // — and it is still the binding in force.
    using Root = A.Root;

    public class NamedChild : Root {}
}
