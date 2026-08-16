import { BinaryCodec } from '../api/base';

export abstract class AbstractCodec implements BinaryCodec {}

/** Same simple name as api/base.ts's TextCodec, and in the binary closure. */
export class TextCodec extends AbstractCodec {}
