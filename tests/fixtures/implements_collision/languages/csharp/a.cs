namespace P
{
    public interface Root { }

    public interface Other { }

    public class Leaf : Root { }

    /// A qualified base, a generic one, and a constraint clause after them.
    public class Hard<T> : System.Collections.Generic.List<T>, Root, Other where T : class { }
}
