export class Root {}

// JavaScript hangs the base straight off `class_heritage`, with none of
// TypeScript's `extends_clause` wrapper around it.
export class Leaf extends Root {}

const ns = { Root };

/** A qualified base, which is a member_expression rather than an identifier. */
export class Hard extends ns.Root {}
