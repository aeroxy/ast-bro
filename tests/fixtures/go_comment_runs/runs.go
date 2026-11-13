// Package runs puts several comments on one line, beside a token or a declaration.
package runs

/* block */ // and line
func SameLine() {}

// one
/* two */ /* three */
func ThreeOnTwoLines() {}

/* first
   spans */ // then
func AfterMultiline() {}

type Fields struct { // brace
	AfterBrace     int
	Annotated      int /* a */ // b
	AfterAnnotated int
	/* c */ /* d */
	TwoOnOneLine int
	Trailed      int // x
	// y
	BelowTrailed int
	// e
	/* inline */ Inline int
	/* lead */
	Lead int
}

type Spanning struct {
	X int /* a */ /* b
	*/ // c
	Y int
	Z int /* z1
	*/ /* z2 */
	W int
}

type Opened struct { /* u1
	*/ // u2
	V int
}

type Separated struct {
	A int; /* x
	*/ // y
	B int
}

const /* k
*/ // after keyword
AfterKeyword = 1

const ( // paren
	AfterParen = 1
)
