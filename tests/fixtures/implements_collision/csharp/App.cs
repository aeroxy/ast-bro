using Example.Api;

namespace Example.App
{
    // Neither this namespace nor a qualifier says which TextCodec; the
    // using directive is the only thing that does.
    public sealed class UsingCodec : TextCodec { }
}
