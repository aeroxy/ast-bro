package org.example.jdbc;

import org.example.api.TextCodec;

/**
 * Writes the bare name `TextCodec` while its own package declares one. The
 * single-type import wins (JLS 6.4.1), so this implements the api interface.
 */
public final class BoxCodec implements TextCodec {
}
