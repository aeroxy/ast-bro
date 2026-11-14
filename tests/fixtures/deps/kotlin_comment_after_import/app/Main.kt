package org.example.app

import org.example.api.Widget

/**
 * The grammar folds this comment into the `import_header` node above it.
 * Reading the header's raw text therefore produced the import path
 * `org.example.api.Widget\n\n/** … */`, which resolved to nothing.
 */
class Main : Widget
