package org.example.api

interface Codec

interface BinaryCodec : Codec

/** Same simple name as org.example.jdbc.TextCodec, unrelated to it. */
interface TextCodec : Codec

/** Resolves in this package, so it never reaches BinaryCodec. */
interface PrimitiveText : TextCodec
