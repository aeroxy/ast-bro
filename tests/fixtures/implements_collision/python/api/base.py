class Codec:
    pass


class BinaryCodec(Codec):
    pass


class TextCodec(Codec):
    """Same simple name as impl.base.TextCodec, unrelated to it."""


class PrimitiveText(TextCodec):
    """Imports nothing: TextCodec is the one in this module."""
