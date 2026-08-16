namespace App
{
    // `global::` is C#'s escape from namespace lookup, and generated code
    // writes it on nearly every reference.
    public class GlobalChild : global::Anchored.Root {}

    public class PlainChild : Anchored.Root {}
}
