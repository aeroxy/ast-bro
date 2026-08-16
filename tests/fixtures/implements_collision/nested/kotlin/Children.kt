package app

import org.example.TestClass
import org.example.TestClass.NestedClass as N2

/** The import names the outer type; the clause spells the rest. */
class ChildOuter : TestClass.NestedClass.SubNestedClass

/** The rename names neither the qualified nor the package-relative form. */
class ChildAliased : N2.SubNestedClass
