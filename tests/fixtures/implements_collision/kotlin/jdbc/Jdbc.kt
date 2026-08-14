package org.example.jdbc

import org.example.api.BinaryCodec

abstract class AbstractCodec : BinaryCodec

/** Same simple name as org.example.api.TextCodec, and in the binary closure. */
class TextCodec : AbstractCodec()
