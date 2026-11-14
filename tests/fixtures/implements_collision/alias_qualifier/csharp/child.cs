using Api = Example.Other;

namespace X
{
    // The alias does not match the namespace's own last segment, so the
    // qualifier only agrees once the alias is expanded.
    public class Child : Api.Base { }
}
