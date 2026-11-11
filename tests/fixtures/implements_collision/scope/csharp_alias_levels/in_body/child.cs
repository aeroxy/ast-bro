namespace A.B
{
    // An alias in the body of `A.B` is consulted before the types of `A`.
    using Picked = X.Picked;

    public class Child : Picked { }
}
