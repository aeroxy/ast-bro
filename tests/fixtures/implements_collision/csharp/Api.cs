namespace Example.Api
{
    public interface Codec { }

    public interface BinaryCodec : Codec { }

    // Same simple name as Example.Jdbc.TextCodec, unrelated to it.
    public interface TextCodec : Codec { }

    // Resolves within this namespace, so it never reaches BinaryCodec.
    public interface PrimitiveText : TextCodec { }
}
