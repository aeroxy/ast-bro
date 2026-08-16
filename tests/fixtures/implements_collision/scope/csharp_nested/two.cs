using Alias = X.Real.Target;

namespace A
{
    // C# searches each enclosing namespace's members before any alias, so
    // this outranks the file-scope one for anything nested under `A`.
    public interface Alias { }
}

namespace A.B
{
    public class Child : Alias { }
}
