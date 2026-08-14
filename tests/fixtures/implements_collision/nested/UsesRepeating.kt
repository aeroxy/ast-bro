package app

import foo.bar.A
import foo.bar.A.B

/** `B.A.C` is a trailing run of `A.B.A.C`, and the bare receiver `A` must
 *  reach the outer type rather than the one that repeats deeper. */
class Deep : B.A.C

class Shallow : A
