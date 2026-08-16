export interface Codec {}

export interface BinaryCodec extends Codec {}

/** Same simple name as impl/base.ts's TextCodec, unrelated to it. */
export interface TextCodec extends Codec {}

/** Resolves in this module, so it never reaches BinaryCodec. */
export interface PrimitiveText extends TextCodec {}
