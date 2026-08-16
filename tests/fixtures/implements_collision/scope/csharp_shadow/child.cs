using Base = A.Root;

namespace N
{
    // The nested binding is the one in force here.
    using Base = B.Root;

    public class Child : Base {}
}
