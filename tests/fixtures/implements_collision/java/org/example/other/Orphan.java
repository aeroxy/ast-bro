package org.example.other;

/** Neither declares TextCodec nor imports one: the edge cannot be pinned. */
public interface Orphan extends TextCodec {
}
