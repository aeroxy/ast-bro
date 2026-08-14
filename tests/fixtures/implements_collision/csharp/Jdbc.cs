namespace Example.Jdbc
{
    public abstract class AbstractCodec : Example.Api.BinaryCodec { }

    // Same simple name as Example.Api.TextCodec, and in the binary closure.
    public sealed class TextCodec : AbstractCodec { }
}
