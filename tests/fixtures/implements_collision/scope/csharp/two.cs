using Alias = A.Real.Target;

namespace A
{
    // Nothing here is called Alias, so the file-scope alias is what it means.
    public class AChild : Alias { }
}

namespace B
{
    // C# looks in the enclosing namespace before its using aliases, so this
    // Alias is the one, not the file-scope rename.
    public interface Alias { }

    public class BChild : Alias { }
}
