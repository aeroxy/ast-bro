using Alias = X.Alias;

namespace A
{
    namespace B
    {
        // C# reads the enclosing namespace before the file-scope alias, so
        // this is `A.Alias`. The alias is an identity one — the spelling
        // that exists to disambiguate — and the subtype is not in the same
        // file as either candidate.
        public class Child : Alias {}
    }
}
