export interface Root {}

export interface Other {}

export class Leaf implements Root {}

export class Box<T> {
    value?: T;
}

/** `extends` and `implements` together, with a type argument that is a
 *  sibling of the base rather than part of it. */
export class Hard<T> extends Box<T> implements Root, Other {}
