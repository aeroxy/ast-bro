from .base import TextCodec


class BoxCodec(TextCodec):
    """A relative import names one file: the sibling `base`, not `api/base`."""
