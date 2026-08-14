from api.base import BinaryCodec


class AbstractCodec(BinaryCodec):
    pass


class TextCodec(AbstractCodec):
    """Same simple name as api.base.TextCodec, and in the binary closure."""
