// A file-scope alias is consulted after the types of every enclosing
// namespace, so `A.Picked` wins.
using Picked = X.Picked;

namespace A.B
{
    public class Child : Picked { }
}
